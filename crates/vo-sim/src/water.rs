//! A water surface: a height field that obeys the damped 2D wave equation, on the GPU.
//!
//! [`Water`] is a [`Sim`]: drive it with a [`Timeline`](crate::Timeline) at the default
//! 120 Hz tick. Drops are events ([`WaterDrop`]): the caller turns word onsets and voice level
//! into drops with a time, a place, a radius and a strength, so the surface is a pure function
//! of time plus the recorded drop list. The sim never reads live audio.
//!
//! Each tick runs one compute dispatch per substep over the grid (one substep at the default
//! speed). After a seek or a step, [`Water::flush`] submits the recorded work and runs the
//! output pass once, which writes:
//!
//! - [`Water::height_view`]: `Rgba16Float`, `r` = height (a drop of strength 1 starts at 1.0),
//!   `g` = change over the last tick, `a` = 1. Filterable.
//! - [`Water::normal_view`]: `Rgba8Snorm`, `xyz` = the surface normal (`+z` faces the viewer,
//!   slope scaled by [`WaterConfig::normal_scale`]), `w` = 1. Filterable.
//!
//! Both are `width x height` grid cells. Cell `(i, j)` sits at uv `((i + 0.5) / w, (j + 0.5) / h)`,
//! so sampling with uv over the popup lines the field up with the popup.
//!
//! Cost (measured by `tests/gpu_water.rs`, which logs the number): at the default 128x64 grid
//! one tick is a single 16x8-workgroup dispatch over 8192 cells, and the output pass is one
//! more. The state is two `f32` buffers, so one snapshot holds `2 * w * h * 4` bytes: 64 KiB at
//! 128x64, 512 such snapshots fit the default 32 MiB cap. A snapshot is a GPU-side buffer copy
//! and never comes back to the CPU.
//!
//! Reduced motion ([`WaterConfig::reduced_motion`], default from `VO_REDUCED_MOTION=1`): drops
//! are ignored and no tick is dispatched, so the surface stays flat at 0.
//!
//! The update per cell, with `c` this tick, `p` the last tick and `L` the 4-neighbour
//! Laplacian with closed edges: `next = (c + (c - p) * vel_keep + c2 * L) * height_keep`.
//! `c2 = (speed * dt / cell)^2`, kept under 0.45 by splitting the tick into substeps, so a
//! finer grid or a faster speed stays stable. The tick count at time `t` depends only on `t`
//! (the timeline's fixed tick), so frame rate never changes the state.

use std::cell::RefCell;

use crate::Sim;

/// The grid at the default 600x300 popup.
pub const DEFAULT_GRID: (u32, u32) = (128, 64);

/// Above this `c2` one tick is split into substeps (explicit 2D wave is stable below 0.5).
const MAX_C2: f64 = 0.45;

/// Drops recorded between two submits, each with its own uniform slot.
const SPLASH_SLOTS: usize = 64;
/// Dynamic uniform offsets must be multiples of 256.
const SPLASH_STRIDE: u64 = 256;

const WG: u32 = 8;

/// How the water behaves. Every field has a default that suits a 600x300 popup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterConfig {
    /// Grid cells across. Default 128.
    pub width: u32,
    /// Grid cells down. Default 64.
    pub height: u32,
    /// Wave speed in grid widths per second, so the look does not change with the grid.
    /// Default 0.3 (180 px/s across a 600 px popup).
    pub speed: f32,
    /// Seconds for a wave's motion to fall to 1/e. Default 0.8.
    pub damping_secs: f32,
    /// Seconds for the surface's mean height to fall to 1/e, so the water settles flat even
    /// though closed edges keep the volume. Default 4.
    pub flatten_secs: f32,
    /// Slope scale for the normal texture: 1 means a height step of 1 per cell is a 45 degree
    /// slope. Default 4.
    pub normal_scale: f32,
    /// Ignore drops and keep the surface flat. Default: `VO_REDUCED_MOTION=1`.
    pub reduced_motion: bool,
}

impl Default for WaterConfig {
    fn default() -> WaterConfig {
        WaterConfig {
            width: DEFAULT_GRID.0,
            height: DEFAULT_GRID.1,
            speed: 0.3,
            damping_secs: 0.8,
            flatten_secs: 4.0,
            normal_scale: 4.0,
            reduced_motion: std::env::var("VO_REDUCED_MOTION").is_ok_and(|v| v == "1"),
        }
    }
}

impl WaterConfig {
    /// The default config with a grid of about one cell per 4.7 px (128x64 at 600x300),
    /// keeping the popup's aspect.
    pub fn for_size(px_width: f32, px_height: f32) -> WaterConfig {
        let cell = 600.0 / DEFAULT_GRID.0 as f32;
        let w = (px_width / cell).round().clamp(8.0, 1024.0) as u32;
        let h = (px_height / cell).round().clamp(8.0, 1024.0) as u32;
        WaterConfig { width: w, height: h, ..WaterConfig::default() }
    }
}

/// A drop on the surface: a smooth bump that spreads as a ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterDrop {
    /// Centre in uv, 0..1 across the popup.
    pub x: f32,
    /// Centre in uv, 0..1 down the popup.
    pub y: f32,
    /// Radius as a fraction of the popup's width.
    pub radius: f32,
    /// Peak height of the bump. Negative pushes the surface down.
    pub strength: f32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuParams {
    width: u32,
    height: u32,
    c2: f32,
    vel_keep: f32,
    height_keep: f32,
    normal_scale: f32,
    _pad: [f32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuSplash {
    x: f32,
    y: f32,
    radius: f32,
    strength: f32,
}

/// The per-tick constants for one `dt`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct TickConsts {
    dt: f64,
    substeps: u32,
    params: [f32; 3],
}

impl TickConsts {
    fn new(cfg: &WaterConfig, dt: f64) -> TickConsts {
        let cells_per_s = cfg.speed as f64 * cfg.width as f64;
        let c2_tick = (cells_per_s * dt).powi(2);
        let substeps = ((c2_tick / MAX_C2).sqrt().ceil() as u32).max(1);
        let sub_dt = dt / substeps as f64;
        let c2 = c2_tick / (substeps as f64).powi(2);
        let vel_keep = (-sub_dt / cfg.damping_secs.max(1e-3) as f64).exp();
        let height_keep = (-sub_dt / cfg.flatten_secs.max(1e-3) as f64).exp();
        TickConsts { dt, substeps, params: [c2 as f32, vel_keep as f32, height_keep as f32] }
    }
}

/// Work recorded but not yet submitted.
#[derive(Default)]
struct Pending {
    enc: Option<wgpu::CommandEncoder>,
    splashes: Vec<GpuSplash>,
    /// The heights changed since the last output pass.
    dirty: bool,
}

/// A saved water state: both height buffers, on the GPU.
pub struct WaterSnapshot {
    buf: wgpu::Buffer,
}

/// What one tick and the output pass cost on the GPU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WaterCost {
    /// GPU time per tick, averaged over the ticks measured.
    pub tick_ms: f64,
    /// GPU time of one output pass (height and normal textures).
    pub output_ms: f64,
}

/// The GPU water surface. See the module docs.
pub struct Water {
    device: wgpu::Device,
    queue: wgpu::Queue,
    cfg: WaterConfig,
    consts: TickConsts,
    params: wgpu::Buffer,
    heights: [wgpu::Buffer; 2],
    /// `binds[k]`: buffer `k` is `cur`, the other `prev`.
    binds: [wgpu::BindGroup; 2],
    splash_buf: wgpu::Buffer,
    splash_bind: wgpu::BindGroup,
    out_bind: wgpu::BindGroup,
    step_pipe: wgpu::ComputePipeline,
    drop_pipe: wgpu::ComputePipeline,
    out_pipe: wgpu::ComputePipeline,
    height_tex: wgpu::Texture,
    normal_tex: wgpu::Texture,
    height_view: wgpu::TextureView,
    normal_view: wgpu::TextureView,
    /// Which buffer is `cur`.
    cur: usize,
    pending: RefCell<Pending>,
}

impl Water {
    /// A flat surface. `device` and `queue` are cloned handles.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, cfg: WaterConfig) -> Water {
        assert!(cfg.width > 0 && cfg.height > 0, "water grid must not be empty");
        let cells = cfg.width as u64 * cfg.height as u64;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-water"),
            source: wgpu::ShaderSource::Wgsl(include_str!("wgsl/water.wgsl").into()),
        });
        let s = wgpu::ShaderStages::COMPUTE;
        let storage = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: s,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: false },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let state_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-water-state"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: s,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                storage(1),
                storage(2),
            ],
        });
        let splash_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-water-splash"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: s,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(size_of::<GpuSplash>() as u64),
                },
                count: None,
            }],
        });
        let tex_entry = |binding, format| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: s,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let out_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-water-out"),
            entries: &[tex_entry(0, wgpu::TextureFormat::Rgba16Float), tex_entry(1, wgpu::TextureFormat::Rgba8Snorm)],
        });
        let pipe = |label: &str, entry: &str, layouts: &[Option<&wgpu::BindGroupLayout>]| {
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(label),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(label),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let step_pipe = pipe("vo-water-step", "wave_step", &[Some(&state_layout)]);
        let drop_pipe = pipe("vo-water-drop", "add_drop", &[Some(&state_layout), Some(&splash_layout)]);
        let out_pipe = pipe("vo-water-out", "output", &[Some(&state_layout), Some(&out_layout)]);

        let consts = TickConsts::new(&cfg, 1.0 / crate::DEFAULT_TICK_HZ);
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-water-params"),
            size: size_of::<GpuParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let height_buf = |label| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: cells * 4,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: true,
            })
        };
        // Mapped at creation: wgpu zero-fills it, so the start state is flat without a pass.
        let heights = [height_buf("vo-water-h0"), height_buf("vo-water-h1")];
        for b in &heights {
            b.unmap();
        }
        let bind = |c: usize| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("vo-water-state"),
                layout: &state_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 1, resource: heights[c].as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: heights[1 - c].as_entire_binding() },
                ],
            })
        };
        let binds = [bind(0), bind(1)];
        let splash_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-water-splash"),
            size: SPLASH_STRIDE * SPLASH_SLOTS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let splash_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-water-splash"),
            layout: &splash_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &splash_buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<GpuSplash>() as u64),
                }),
            }],
        });
        let tex = |label, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: cfg.width, height: cfg.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let height_tex = tex("vo-water-height", wgpu::TextureFormat::Rgba16Float);
        let normal_tex = tex("vo-water-normal", wgpu::TextureFormat::Rgba8Snorm);
        let height_view = height_tex.create_view(&Default::default());
        let normal_view = normal_tex.create_view(&Default::default());
        let out_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-water-out"),
            layout: &out_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&height_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&normal_view) },
            ],
        });
        let water = Water {
            device: device.clone(),
            queue: queue.clone(),
            cfg,
            consts,
            params,
            heights,
            binds,
            splash_buf,
            splash_bind,
            out_bind,
            step_pipe,
            drop_pipe,
            out_pipe,
            height_tex,
            normal_tex,
            height_view,
            normal_view,
            cur: 0,
            pending: RefCell::new(Pending { dirty: true, ..Default::default() }),
        };
        water.write_params();
        water
    }

    pub fn config(&self) -> &WaterConfig {
        &self.cfg
    }

    /// Substeps per tick at the current tick rate (1 at the defaults).
    pub fn substeps(&self) -> u32 {
        self.consts.substeps
    }

    /// The height texture (see the module docs). Call [`Water::flush`] after moving the
    /// timeline and before sampling it.
    pub fn height_view(&self) -> &wgpu::TextureView {
        &self.height_view
    }

    /// The normal texture (see the module docs). Call [`Water::flush`] first.
    pub fn normal_view(&self) -> &wgpu::TextureView {
        &self.normal_view
    }

    pub fn height_texture(&self) -> &wgpu::Texture {
        &self.height_tex
    }

    pub fn normal_texture(&self) -> &wgpu::Texture {
        &self.normal_tex
    }

    /// Bytes one snapshot holds: both height buffers.
    pub fn snapshot_size(&self) -> usize {
        self.cells() * 8
    }

    fn cells(&self) -> usize {
        self.cfg.width as usize * self.cfg.height as usize
    }

    fn write_params(&self) {
        let [c2, vel_keep, height_keep] = self.consts.params;
        let p = GpuParams {
            width: self.cfg.width,
            height: self.cfg.height,
            c2,
            vel_keep,
            height_keep,
            normal_scale: self.cfg.normal_scale,
            _pad: [0.0; 2],
        };
        self.queue.write_buffer(&self.params, 0, bytemuck::bytes_of(&p));
    }

    fn groups(&self) -> (u32, u32) {
        (self.cfg.width.div_ceil(WG), self.cfg.height.div_ceil(WG))
    }

    /// Runs `f` on the pending encoder, creating it when there is none.
    fn record<R>(&self, f: impl FnOnce(&mut wgpu::CommandEncoder) -> R) -> R {
        let mut p = self.pending.borrow_mut();
        let enc = p.enc.get_or_insert_with(|| {
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vo-water") })
        });
        f(enc)
    }

    /// One compute pass with one dispatch. A pass each keeps every dispatch ordered after the
    /// last one's writes.
    fn dispatch(
        &self,
        enc: &mut wgpu::CommandEncoder,
        pipe: &wgpu::ComputePipeline,
        group1: Option<(&wgpu::BindGroup, &[u32])>,
        ts: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let (gx, gy) = self.groups();
        let mut pass =
            enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vo-water"), timestamp_writes: ts });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &self.binds[self.cur], &[]);
        if let Some((bind, offsets)) = group1 {
            pass.set_bind_group(1, bind, offsets);
        }
        pass.dispatch_workgroups(gx, gy, 1);
    }

    /// Records one tick's substeps.
    fn record_tick(
        &mut self,
        ts_first: Option<wgpu::ComputePassTimestampWrites<'_>>,
        ts_last: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let n = self.consts.substeps;
        let mut first = ts_first;
        let mut last = ts_last;
        for i in 0..n {
            let ts = match (i == 0, i + 1 == n) {
                (true, true) => match (first.take(), last.take()) {
                    (Some(a), Some(b)) => Some(wgpu::ComputePassTimestampWrites {
                        query_set: a.query_set,
                        beginning_of_pass_write_index: a.beginning_of_pass_write_index,
                        end_of_pass_write_index: b.end_of_pass_write_index,
                    }),
                    (a, b) => a.or(b),
                },
                (true, false) => first.take(),
                (false, true) => last.take(),
                _ => None,
            };
            self.record(|enc| self.dispatch(enc, &self.step_pipe, None, ts));
            self.cur = 1 - self.cur;
        }
        self.pending.borrow_mut().dirty = true;
    }

    /// Submits the recorded work and, when the heights changed, refreshes the height and
    /// normal textures. Call it after moving the timeline and before sampling the textures.
    pub fn flush(&self) {
        let dirty = self.pending.borrow().dirty;
        if dirty {
            self.record(|enc| self.dispatch(enc, &self.out_pipe, Some((&self.out_bind, &[])), None));
        }
        self.submit();
    }

    /// Writes the drop uniforms and submits the pending encoder, without the output pass.
    fn submit(&self) {
        let mut p = self.pending.borrow_mut();
        if !p.splashes.is_empty() {
            let mut bytes = vec![0u8; SPLASH_STRIDE as usize * p.splashes.len()];
            for (i, s) in p.splashes.iter().enumerate() {
                let at = i * SPLASH_STRIDE as usize;
                bytes[at..at + size_of::<GpuSplash>()].copy_from_slice(bytemuck::bytes_of(s));
            }
            self.queue.write_buffer(&self.splash_buf, 0, &bytes);
            p.splashes.clear();
        }
        if let Some(enc) = p.enc.take() {
            self.queue.submit([enc.finish()]);
        }
        p.dirty = false;
    }

    /// Reads the current heights back (the `cur` buffer, row by row). Waits for the GPU.
    pub fn read_heights(&self) -> Vec<f32> {
        self.flush();
        let bytes = self.cells() as u64 * 4;
        let rb = self.readback_buffer(bytes);
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(&self.heights[self.cur], 0, &rb, 0, bytes);
        self.queue.submit([enc.finish()]);
        self.map(&rb).as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)).collect()
    }

    /// Reads the height texture back as tightly packed `Rgba16Float` rows (8 bytes a cell).
    pub fn read_height_texture(&self) -> Vec<u8> {
        self.flush();
        let (w, h) = (self.cfg.width, self.cfg.height);
        let row = (w * 8).next_multiple_of(256);
        let rb = self.readback_buffer(row as u64 * h as u64);
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            self.height_tex.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &rb,
                layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row), rows_per_image: Some(h) },
            },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let data = self.map(&rb);
        data.chunks(row as usize).flat_map(|r| r[..(w * 8) as usize].to_vec()).collect()
    }

    fn readback_buffer(&self, size: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-water-readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn map(&self, buf: &wgpu::Buffer) -> Vec<u8> {
        buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map water readback"));
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).expect("poll the GPU");
        let data = buf.slice(..).get_mapped_range().expect("read the mapped water buffer").to_vec();
        buf.unmap();
        data
    }

    /// Measures the GPU time of `ticks` ticks and one output pass on the current state, then
    /// puts the state back. `None` when the device has no `TIMESTAMP_QUERY`.
    pub fn gpu_cost(&mut self, ticks: u32) -> Option<WaterCost> {
        if !self.device.features().contains(wgpu::Features::TIMESTAMP_QUERY) || ticks == 0 {
            return None;
        }
        self.flush();
        let saved = self.snapshot();
        let set = self.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("vo-water-cost"),
            ty: wgpu::QueryType::Timestamp,
            count: 4,
        });
        let ts = |begin: Option<u32>, end: Option<u32>| {
            Some(wgpu::ComputePassTimestampWrites {
                query_set: &set,
                beginning_of_pass_write_index: begin,
                end_of_pass_write_index: end,
            })
        };
        for i in 0..ticks {
            let first = if i == 0 { ts(Some(0), None) } else { None };
            let last = if i + 1 == ticks { ts(None, Some(1)) } else { None };
            self.record_tick(first, last);
        }
        self.record(|enc| self.dispatch(enc, &self.out_pipe, Some((&self.out_bind, &[])), ts(Some(2), Some(3))));
        let resolve = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-water-cost"),
            size: 32,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let rb = self.readback_buffer(32);
        self.record(|enc| {
            enc.resolve_query_set(&set, 0..4, &resolve, 0);
            enc.copy_buffer_to_buffer(&resolve, 0, &rb, 0, 32);
        });
        self.submit();
        let stamps: Vec<u64> = self.map(&rb).as_chunks::<8>().0.iter().map(|b| u64::from_le_bytes(*b)).collect();
        let period = self.queue.get_timestamp_period() as f64 * 1e-6;
        self.restore(&saved);
        self.flush();
        Some(WaterCost {
            tick_ms: stamps[1].saturating_sub(stamps[0]) as f64 * period / ticks as f64,
            output_ms: stamps[3].saturating_sub(stamps[2]) as f64 * period,
        })
    }
}

impl Sim for Water {
    type Event = WaterDrop;
    type Snapshot = WaterSnapshot;

    fn reset(&mut self) {
        self.record(|enc| {
            enc.clear_buffer(&self.heights[0], 0, None);
            enc.clear_buffer(&self.heights[1], 0, None);
        });
        self.cur = 0;
        self.pending.borrow_mut().dirty = true;
    }

    fn step(&mut self, _tick: u64, dt: f64) {
        if self.consts.dt != dt {
            // The params uniform is written at the next submit, so finish the work recorded
            // under the old one first.
            self.submit();
            self.pending.borrow_mut().dirty = true;
            self.consts = TickConsts::new(&self.cfg, dt);
            self.write_params();
        }
        if self.cfg.reduced_motion {
            return;
        }
        self.record_tick(None, None);
    }

    fn apply(&mut self, e: &WaterDrop) {
        if self.cfg.reduced_motion {
            return;
        }
        if self.pending.borrow().splashes.len() == SPLASH_SLOTS {
            self.submit();
            self.pending.borrow_mut().dirty = true;
        }
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        let slot = {
            let mut p = self.pending.borrow_mut();
            p.splashes.push(GpuSplash {
                x: e.x * w - 0.5,
                y: e.y * h - 0.5,
                radius: (e.radius * w).max(0.5),
                strength: e.strength,
            });
            p.dirty = true;
            p.splashes.len() - 1
        };
        let offset = [(slot as u64 * SPLASH_STRIDE) as u32];
        self.record(|enc| self.dispatch(enc, &self.drop_pipe, Some((&self.splash_bind, &offset)), None));
    }

    fn snapshot(&mut self) -> WaterSnapshot {
        let bytes = self.cells() as u64 * 4;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-water-snapshot"),
            size: bytes * 2,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.record(|enc| {
            enc.copy_buffer_to_buffer(&self.heights[self.cur], 0, &buf, 0, bytes);
            enc.copy_buffer_to_buffer(&self.heights[1 - self.cur], 0, &buf, bytes, bytes);
        });
        WaterSnapshot { buf }
    }

    fn restore(&mut self, snap: &WaterSnapshot) {
        let bytes = self.cells() as u64 * 4;
        self.record(|enc| {
            enc.copy_buffer_to_buffer(&snap.buf, 0, &self.heights[0], 0, bytes);
            enc.copy_buffer_to_buffer(&snap.buf, bytes, &self.heights[1], 0, bytes);
        });
        self.cur = 0;
        self.pending.borrow_mut().dirty = true;
    }

    fn snapshot_bytes(&self, _snap: &WaterSnapshot) -> usize {
        self.snapshot_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substeps_keep_the_wave_stable() {
        let cfg = WaterConfig { reduced_motion: false, ..WaterConfig::default() };
        let one = TickConsts::new(&cfg, 1.0 / 120.0);
        assert_eq!(one.substeps, 1);
        assert!(one.params[0] < MAX_C2 as f32);
        let fine = WaterConfig { width: 1024, height: 512, ..cfg };
        let many = TickConsts::new(&fine, 1.0 / 120.0);
        assert!(many.substeps > 1);
        assert!(many.params[0] <= MAX_C2 as f32 + 1e-6);
    }

    #[test]
    fn grid_follows_the_popup_size() {
        let c = WaterConfig::for_size(600.0, 300.0);
        assert_eq!((c.width, c.height), DEFAULT_GRID);
        let c = WaterConfig::for_size(900.0, 150.0);
        assert_eq!((c.width, c.height), (192, 32));
    }
}
