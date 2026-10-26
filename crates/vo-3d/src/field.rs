//! [`TextField`]: a distance field of the current text (TY19), rebuilt each frame so 3D
//! scenes and particles can react to the words without reading the text layout (CAPABILITIES
//! section 3, item 8).
//!
//! The text display list is drawn by the normal 2D renderer (Slug coverage) at half
//! resolution into a float target, then a jump flood finds, for every texel, the nearest
//! inside and outside texel. The result is one `Rgba16Float` texture: signed distance in
//! target pixels (negative inside the ink), the unit direction away from the nearest ink, and
//! the coverage. Coverage is alpha, so text drawn at partial opacity counts as thinner ink.

use std::time::{Duration, Instant};

use bytemuck::{Pod, Zeroable};
use vo_render::{Renderer, Target};
use vo_scene::display::DisplayList;

use crate::render::FieldBinding;

const FIELD_WGSL: &str = include_str!("wgsl/field.wgsl");
const SLOT: u64 = 256;

pub fn shader_source() -> &'static str {
    FIELD_WGSL
}

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct Params {
    size: [u32; 2],
    step: u32,
    scale: f32,
}

struct Sized {
    size: [u32; 2],
    coverage: Target,
    /// Flood textures, held for the bind groups.
    _ping: [wgpu::Texture; 2],
    field: wgpu::Texture,
    field_view: wgpu::TextureView,
    /// Bind groups: seed, one per step, resolve.
    binds: Vec<wgpu::BindGroup>,
    steps: Vec<u32>,
}

pub struct TextField {
    device: wgpu::Device,
    queue: wgpu::Queue,
    seed: wgpu::ComputePipeline,
    step: wgpu::ComputePipeline,
    resolve: wgpu::ComputePipeline,
    params: wgpu::Buffer,
    sized: Option<Sized>,
    /// Target pixels per field texel.
    downscale: f32,
    pub pipeline_time: Duration,
}

impl TextField {
    /// A field at half the target resolution.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> TextField {
        TextField::with_downscale(device, queue, 2.0)
    }

    /// A field with `downscale` target pixels per field texel.
    pub fn with_downscale(device: &wgpu::Device, queue: &wgpu::Queue, downscale: f32) -> TextField {
        let start = Instant::now();
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo3d-field"),
            source: wgpu::ShaderSource::Wgsl(FIELD_WGSL.into()),
        });
        let pipe = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: None,
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let (seed, step, resolve) = (pipe("cs_seed"), pipe("cs_step"), pipe("cs_resolve"));
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo3d-field-params"),
            size: SLOT * 32,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        TextField {
            device: device.clone(),
            queue: queue.clone(),
            seed,
            step,
            resolve,
            params,
            sized: None,
            downscale,
            pipeline_time: start.elapsed(),
        }
    }

    /// Records drawing `text` with `renderer` at `scale / downscale`, then the flood, into
    /// `enc`. `target_size` is the size in physical pixels of the target the field will be
    /// used with. The renderer's buffers are written now, so submit `enc` before the renderer
    /// encodes another list.
    pub fn update(
        &mut self,
        renderer: &mut Renderer,
        text: &DisplayList,
        target_size: [u32; 2],
        scale: f32,
        enc: &mut wgpu::CommandEncoder,
    ) {
        let size = target_size.map(|v| ((v as f32 / self.downscale).ceil() as u32).max(1));
        self.resize(size);
        let s = self.sized.as_ref().unwrap();
        renderer.encode(text, scale / self.downscale, &s.coverage, enc);
        self.encode_flood(enc);
    }

    /// Records the seed, flood and resolve passes. The coverage must already be drawn.
    fn encode_flood(&self, enc: &mut wgpu::CommandEncoder) {
        let s = self.sized.as_ref().unwrap();
        let groups = [s.size[0].div_ceil(8), s.size[1].div_ceil(8)];
        let mut pass =
            enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vo3d-field"), timestamp_writes: None });
        let n = s.binds.len();
        for (i, bind) in s.binds.iter().enumerate() {
            let pipe = if i == 0 {
                &self.seed
            } else if i == n - 1 {
                &self.resolve
            } else {
                &self.step
            };
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, bind, &[]);
            pass.dispatch_workgroups(groups[0], groups[1], 1);
        }
    }

    fn resize(&mut self, size: [u32; 2]) {
        if self.sized.as_ref().is_some_and(|s| s.size == size) {
            return;
        }
        let d = &self.device;
        let tex = |label, format, usage| {
            d.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        use wgpu::TextureUsages as U;
        let coverage = Target::new(d, size[0], size[1], wgpu::TextureFormat::Rgba16Float);
        let ping =
            [0, 1].map(|_| tex("vo3d-jfa", wgpu::TextureFormat::Rgba32Float, U::STORAGE_BINDING | U::TEXTURE_BINDING));
        let field =
            tex("vo3d-field", wgpu::TextureFormat::Rgba16Float, U::STORAGE_BINDING | U::TEXTURE_BINDING | U::COPY_SRC);
        let field_view = field.create_view(&Default::default());

        // Step widths: half the larger side rounded up to a power of two, down to 1, then a
        // final 1 (JFA+1), which fixes most of the flood's rare misses.
        let mut steps = Vec::new();
        let mut k = size[0].max(size[1]).next_power_of_two() / 2;
        while k >= 1 {
            steps.push(k);
            k /= 2;
        }
        steps.push(1);

        // Params: slot 0 for seed and resolve (step unused), then one per step.
        let scale = self.downscale;
        let mut bytes = Vec::new();
        for step in std::iter::once(0).chain(steps.iter().copied()) {
            let start = bytes.len();
            bytes.extend_from_slice(bytemuck::bytes_of(&Params { size, step, scale }));
            bytes.resize(start + SLOT as usize, 0);
        }
        assert!(bytes.len() as u64 <= self.params.size(), "field too large for the step table");
        self.queue.write_buffer(&self.params, 0, &bytes);

        let views = ping.each_ref().map(|t| t.create_view(&Default::default()));
        let cov_view = coverage.texture().create_view(&Default::default());
        let param = |slot: u64| {
            wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer: &self.params,
                offset: slot * SLOT,
                size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
            })
        };
        let view = |v| wgpu::BindingResource::TextureView(v);
        let mut binds = Vec::new();
        binds.push(d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-field-seed"),
            layout: &self.seed.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: param(0) },
                wgpu::BindGroupEntry { binding: 1, resource: view(&cov_view) },
                wgpu::BindGroupEntry { binding: 3, resource: view(&views[0]) },
            ],
        }));
        // The seed writes ping 0; step i reads ping i % 2 and writes the other.
        for i in 0..steps.len() {
            binds.push(d.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo3d-field-step"),
                layout: &self.step.get_bind_group_layout(0),
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: param(1 + i as u64) },
                    wgpu::BindGroupEntry { binding: 2, resource: view(&views[i % 2]) },
                    wgpu::BindGroupEntry { binding: 3, resource: view(&views[(i + 1) % 2]) },
                ],
            }));
        }
        binds.push(d.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo3d-field-resolve"),
            layout: &self.resolve.get_bind_group_layout(0),
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: param(0) },
                wgpu::BindGroupEntry { binding: 1, resource: view(&cov_view) },
                wgpu::BindGroupEntry { binding: 2, resource: view(&views[steps.len() % 2]) },
                wgpu::BindGroupEntry { binding: 4, resource: view(&field_view) },
            ],
        }));
        self.sized = Some(Sized { size, coverage, _ping: ping, field, field_view, binds, steps });
    }

    /// The field for [`crate::Renderer3d::encode`], once [`TextField::update`] has run.
    pub fn binding(&self) -> Option<FieldBinding<'_>> {
        let s = self.sized.as_ref()?;
        Some(FieldBinding { view: &s.field_view, size: s.size, scale: self.downscale })
    }

    /// The field texture, for readback and debugging.
    pub fn texture(&self) -> Option<&wgpu::Texture> {
        self.sized.as_ref().map(|s| &s.field)
    }

    /// Number of flood steps at the current size.
    pub fn steps(&self) -> usize {
        self.sized.as_ref().map_or(0, |s| s.steps.len())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn field_shader_validates_in_naga() {
        let src = super::shader_source();
        let module = naga::front::wgsl::parse_str(src).unwrap_or_else(|e| panic!("{}", e.emit_to_string(src)));
        naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::empty())
            .validate(&module)
            .unwrap_or_else(|e| panic!("{e:?}"));
    }
}
