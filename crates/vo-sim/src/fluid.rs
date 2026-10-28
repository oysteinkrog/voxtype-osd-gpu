//! Ink and smoke: a stable-fluids grid with velocity and dye, on the GPU.
//!
//! [`Fluid`] is a [`Sim`]: drive it with a [`Timeline`](crate::Timeline) at the default
//! 120 Hz tick. Pushes and dye are events ([`FluidSplat`]): the caller turns word onsets and
//! voice level into splats with a time, a place, a radius, a push and a colour, so the flow is
//! a pure function of time plus the recorded splat list. The sim never reads live audio. A
//! dye source that runs for a while is a run of splats, one per tick or per word.
//!
//! One tick, in order (one compute pass each, fixed work every tick):
//!
//! 1. curl of the velocity;
//! 2. vorticity confinement ([`FluidConfig::curl`]) and the velocity fade;
//! 3. semi-Lagrangian advection of the velocity by itself (bilinear back-trace);
//! 4. divergence;
//! 5. [`FluidConfig::pressure_iterations`] red-black SOR sweeps (two dispatches each),
//!    warm-started from the last tick's pressure. The count is fixed, so the result never
//!    depends on convergence timing. Red-black, so each half sweep reads only cells it does
//!    not write;
//! 6. subtract the pressure gradient, close the walls;
//! 7. semi-Lagrangian advection of the dye;
//! 8. the dye total put back ([`FluidConfig::conserve_dye`]) and the dye fade.
//!
//! Walls are closed: nothing flows out. Even so, semi-Lagrangian advection does not keep the
//! dye total by itself. On this collocated grid the projection solves with the compact
//! 5-point Laplacian while divergence and gradient use central differences, so about 1/s of
//! divergence is left where a push is sharp. Without step 8, a 0.6 widths/s push loses a third
//! of its dye in 3 s. Step 8 sums the dye before and after advection (one workgroup each, in
//! a fixed order, so the sum is the same bits every run) and scales all the dye by the ratio.
//! The scale is uniform, so it never moves the dye's centre of mass. A staggered (MAC) grid
//! would fix the cause and is the upgrade path if local thinning ever shows.
//!
//! After a seek or a step, [`Fluid::flush`] submits the recorded work and runs the output pass
//! once, which writes:
//!
//! - [`Fluid::dye_view`]: `Rgba16Float`, premultiplied `rgb` and `a` = dye amount (a splat of
//!   amount 1 gives 1.0 at its centre; overlapping splats can go above 1). Filterable.
//! - [`Fluid::velocity_view`]: `Rgba16Float`, `xy` = velocity in uv per second (`x` in popup
//!   widths, `y` in popup heights), `z` = curl in radians per second, `w` = 1. For smears.
//!
//! Both are `width x height` grid cells. Cell `(i, j)` sits at uv `((i + 0.5) / w, (j + 0.5) / h)`.
//!
//! Cost (measured by `tests/gpu_fluid.rs`, which logs the number): at the default 128x64 grid
//! one tick is `7 + 2 * pressure_iterations` dispatches (47 at the default 20 sweeps) of 16x8
//! workgroups, plus two single-workgroup sums. Measured on the dev box's GPU: 0.34 ms per
//! tick, almost all of it per-dispatch overhead on so small a grid, so fewer sweeps
//! ([`FluidConfig::pressure_iterations`]) is the lever when the budget is tight. The output
//! pass costs 0.003 ms. One snapshot holds velocity (8 bytes a cell), dye (16) and
//! pressure (4): `28 * w * h` bytes, 224 KiB at 128x64, so 146 snapshots (146 s at one a
//! second) fit the default 32 MiB cap before thinning. A snapshot is a GPU-side buffer copy and
//! never comes back to the CPU.
//!
//! Reduced motion ([`FluidConfig::reduced_motion`], default from `VO_REDUCED_MOTION=1`): the
//! push of every splat is dropped and no tick is dispatched, so dye lands where it is placed
//! and stays still.

use std::cell::RefCell;

use crate::Sim;

/// The grid at the default 600x300 popup.
pub const DEFAULT_GRID: (u32, u32) = (128, 64);

/// Splats recorded between two submits, each with its own uniform slot.
const SPLAT_SLOTS: usize = 64;
/// Dynamic uniform offsets must be multiples of 256.
const SPLAT_STRIDE: u64 = 256;

const WG: u32 = 8;

/// Over-relaxation of the pressure solve. Near the best for a 128x64 grid; below 2 it is stable.
const SOR: f32 = 1.85;

/// How the fluid behaves. Every field has a default that suits a 600x300 popup.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidConfig {
    /// Grid cells across. Default 128.
    pub width: u32,
    /// Grid cells down. Default 64.
    pub height: u32,
    /// Red-black SOR sweeps of the pressure solve per tick, at least 1. Default 20.
    pub pressure_iterations: u32,
    /// Vorticity confinement strength: how hard small swirls are kept curling. 0 turns it off.
    /// Default 4.
    pub curl: f32,
    /// Seconds for the flow's speed to fall to 1/e, so a push coasts and stops. Default 1.5.
    pub velocity_fade_secs: f32,
    /// Seconds for the dye to fall to 1/e. 0 keeps dye forever (ink). Default 0.
    pub dye_fade_secs: f32,
    /// Put the dye total back after each advection (see the module docs). Default true.
    pub conserve_dye: bool,
    /// Drop every push and keep the dye still. Default: `VO_REDUCED_MOTION=1`.
    pub reduced_motion: bool,
}

impl Default for FluidConfig {
    fn default() -> FluidConfig {
        FluidConfig {
            width: DEFAULT_GRID.0,
            height: DEFAULT_GRID.1,
            pressure_iterations: 20,
            curl: 4.0,
            velocity_fade_secs: 1.5,
            dye_fade_secs: 0.0,
            conserve_dye: true,
            reduced_motion: std::env::var("VO_REDUCED_MOTION").is_ok_and(|v| v == "1"),
        }
    }
}

impl FluidConfig {
    /// The default config with a grid of about one cell per 4.7 px (128x64 at 600x300),
    /// keeping the popup's aspect.
    pub fn for_size(px_width: f32, px_height: f32) -> FluidConfig {
        let cell = 600.0 / DEFAULT_GRID.0 as f32;
        let w = (px_width / cell).round().clamp(8.0, 1024.0) as u32;
        let h = (px_height / cell).round().clamp(8.0, 1024.0) as u32;
        FluidConfig { width: w, height: h, ..FluidConfig::default() }
    }

    /// Pressure sweeps actually run: the config's, at least 1.
    pub fn iterations(&self) -> u32 {
        self.pressure_iterations.max(1)
    }
}

/// A splat: a smooth bump of push and dye. Zero `velocity` places dye only; zero `amount`
/// pushes only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidSplat {
    /// Centre in uv, 0..1 across the popup.
    pub x: f32,
    /// Centre in uv, 0..1 down the popup.
    pub y: f32,
    /// Radius as a fraction of the popup's width.
    pub radius: f32,
    /// Push added at the centre, in uv per second (`x` in popup widths, `y` in heights).
    pub velocity: [f32; 2],
    /// Dye colour, straight (not premultiplied), 0..1.
    pub color: [f32; 3],
    /// Dye amount added at the centre. 1 is full.
    pub amount: f32,
}

impl FluidSplat {
    /// Dye only.
    pub fn dye(x: f32, y: f32, radius: f32, color: [f32; 3], amount: f32) -> FluidSplat {
        FluidSplat { x, y, radius, velocity: [0.0; 2], color, amount }
    }

    /// A push only.
    pub fn push(x: f32, y: f32, radius: f32, velocity: [f32; 2]) -> FluidSplat {
        FluidSplat { x, y, radius, velocity, color: [0.0; 3], amount: 0.0 }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuParams {
    width: u32,
    height: u32,
    dt: f32,
    vel_keep: f32,
    dye_keep: f32,
    curl: f32,
    sor: f32,
    conserve: u32,
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GpuSplat {
    x: f32,
    y: f32,
    radius: f32,
    _pad: f32,
    vx: f32,
    vy: f32,
    _pad2: [f32; 2],
    dye: [f32; 4],
}

/// Work recorded but not yet submitted.
#[derive(Default)]
struct Pending {
    enc: Option<wgpu::CommandEncoder>,
    splats: Vec<GpuSplat>,
    /// The state changed since the last output pass.
    dirty: bool,
}

/// A saved fluid state: velocity, dye and pressure, on the GPU.
pub struct FluidSnapshot {
    buf: wgpu::Buffer,
}

/// What one tick and the output pass cost on the GPU.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FluidCost {
    /// GPU time per tick, averaged over the ticks measured.
    pub tick_ms: f64,
    /// GPU time of one output pass (dye and velocity textures).
    pub output_ms: f64,
}

/// The state buffers, in snapshot order.
struct State {
    vel: wgpu::Buffer,
    dye: wgpu::Buffer,
    pres: wgpu::Buffer,
}

struct Pipes {
    curl: wgpu::ComputePipeline,
    confine: wgpu::ComputePipeline,
    advect_vel: wgpu::ComputePipeline,
    divergence: wgpu::ComputePipeline,
    sor_red: wgpu::ComputePipeline,
    sor_black: wgpu::ComputePipeline,
    project: wgpu::ComputePipeline,
    advect_dye: wgpu::ComputePipeline,
    sum_before: wgpu::ComputePipeline,
    sum_after: wgpu::ComputePipeline,
    store_dye: wgpu::ComputePipeline,
    splat: wgpu::ComputePipeline,
    output: wgpu::ComputePipeline,
}

/// The GPU fluid. See the module docs.
pub struct Fluid {
    device: wgpu::Device,
    queue: wgpu::Queue,
    cfg: FluidConfig,
    dt: f64,
    params: wgpu::Buffer,
    state: State,
    /// Velocity, dye and curl/divergence scratch. Every pass writes them before reading,
    /// so they are not part of a snapshot.
    scratch: [wgpu::Buffer; 3],
    bind: wgpu::BindGroup,
    splat_buf: wgpu::Buffer,
    splat_bind: wgpu::BindGroup,
    out_bind: wgpu::BindGroup,
    pipes: Pipes,
    /// Dye totals before and after advection; only the GPU reads them.
    _sums: wgpu::Buffer,
    dye_tex: wgpu::Texture,
    vel_tex: wgpu::Texture,
    dye_view: wgpu::TextureView,
    vel_view: wgpu::TextureView,
    pending: RefCell<Pending>,
}

impl Fluid {
    /// A still, empty fluid. `device` and `queue` are cloned handles.
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, cfg: FluidConfig) -> Fluid {
        assert!(cfg.width > 1 && cfg.height > 1, "fluid grid must be at least 2x2");
        let cells = cfg.width as u64 * cfg.height as u64;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("vo-fluid"),
            source: wgpu::ShaderSource::Wgsl(include_str!("wgsl/fluid.wgsl").into()),
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
        let mut entries = vec![wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: s,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        }];
        entries.extend((1..=7).map(storage));
        let state_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-fluid-state"),
            entries: &entries,
        });
        let splat_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-fluid-splat"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: s,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(size_of::<GpuSplat>() as u64),
                },
                count: None,
            }],
        });
        let tex_entry = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: s,
            ty: wgpu::BindingType::StorageTexture {
                access: wgpu::StorageTextureAccess::WriteOnly,
                format: wgpu::TextureFormat::Rgba16Float,
                view_dimension: wgpu::TextureViewDimension::D2,
            },
            count: None,
        };
        let out_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("vo-fluid-out"),
            entries: &[tex_entry(0), tex_entry(1)],
        });
        let pipe = |entry: &str, layouts: &[Option<&wgpu::BindGroupLayout>]| {
            let label = format!("vo-fluid-{entry}");
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some(&label),
                bind_group_layouts: layouts,
                immediate_size: 0,
            });
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(&label),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        let state_only = [Some(&state_layout)];
        let pipes = Pipes {
            curl: pipe("curl", &state_only),
            confine: pipe("confine", &state_only),
            advect_vel: pipe("advect_vel", &state_only),
            divergence: pipe("divergence", &state_only),
            sor_red: pipe("sor_red", &state_only),
            sor_black: pipe("sor_black", &state_only),
            project: pipe("project", &state_only),
            advect_dye: pipe("advect_dye", &state_only),
            sum_before: pipe("sum_before", &state_only),
            sum_after: pipe("sum_after", &state_only),
            store_dye: pipe("store_dye", &state_only),
            splat: pipe("splat", &[Some(&state_layout), Some(&splat_layout)]),
            output: pipe("output", &[Some(&state_layout), Some(&out_layout)]),
        };

        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-fluid-params"),
            size: size_of::<GpuParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        // Mapped at creation: wgpu zero-fills it, so the start state is still without a pass.
        let buf = |label, bytes_per_cell: u64| {
            let b = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: cells * bytes_per_cell,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: true,
            });
            b.unmap();
            b
        };
        let state = State { vel: buf("vo-fluid-vel", 8), dye: buf("vo-fluid-dye", 16), pres: buf("vo-fluid-pres", 4) };
        let scratch = [buf("vo-fluid-vel-tmp", 8), buf("vo-fluid-dye-tmp", 16), buf("vo-fluid-scratch", 8)];
        let sums = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-fluid-sums"),
            size: 8,
            usage: wgpu::BufferUsages::STORAGE,
            mapped_at_creation: false,
        });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-fluid-state"),
            layout: &state_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: state.vel.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: scratch[0].as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: state.dye.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: scratch[1].as_entire_binding() },
                wgpu::BindGroupEntry { binding: 5, resource: state.pres.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 6, resource: scratch[2].as_entire_binding() },
                wgpu::BindGroupEntry { binding: 7, resource: sums.as_entire_binding() },
            ],
        });
        let splat_buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-fluid-splat"),
            size: SPLAT_STRIDE * SPLAT_SLOTS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let splat_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-fluid-splat"),
            layout: &splat_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 2,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &splat_buf,
                    offset: 0,
                    size: wgpu::BufferSize::new(size_of::<GpuSplat>() as u64),
                }),
            }],
        });
        let tex = |label| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: cfg.width, height: cfg.height, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::STORAGE_BINDING
                    | wgpu::TextureUsages::TEXTURE_BINDING
                    | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            })
        };
        let dye_tex = tex("vo-fluid-dye");
        let vel_tex = tex("vo-fluid-velocity");
        let dye_view = dye_tex.create_view(&Default::default());
        let vel_view = vel_tex.create_view(&Default::default());
        let out_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("vo-fluid-out"),
            layout: &out_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&dye_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&vel_view) },
            ],
        });
        let fluid = Fluid {
            device: device.clone(),
            queue: queue.clone(),
            cfg,
            dt: 1.0 / crate::DEFAULT_TICK_HZ,
            params,
            state,
            scratch,
            bind,
            splat_buf,
            splat_bind,
            out_bind,
            pipes,
            _sums: sums,
            dye_tex,
            vel_tex,
            dye_view,
            vel_view,
            pending: RefCell::new(Pending { dirty: true, ..Default::default() }),
        };
        fluid.write_params();
        fluid
    }

    pub fn config(&self) -> &FluidConfig {
        &self.cfg
    }

    /// The dye texture (see the module docs). Call [`Fluid::flush`] after moving the timeline
    /// and before sampling it.
    pub fn dye_view(&self) -> &wgpu::TextureView {
        &self.dye_view
    }

    /// The velocity texture (see the module docs). Call [`Fluid::flush`] first.
    pub fn velocity_view(&self) -> &wgpu::TextureView {
        &self.vel_view
    }

    pub fn dye_texture(&self) -> &wgpu::Texture {
        &self.dye_tex
    }

    pub fn velocity_texture(&self) -> &wgpu::Texture {
        &self.vel_tex
    }

    /// Bytes one snapshot holds: velocity, dye and pressure.
    pub fn snapshot_size(&self) -> usize {
        self.cells() * 28
    }

    fn cells(&self) -> usize {
        self.cfg.width as usize * self.cfg.height as usize
    }

    /// The state buffers with their sizes, in snapshot order.
    fn state_parts(&self) -> [(&wgpu::Buffer, u64); 3] {
        let c = self.cells() as u64;
        [(&self.state.vel, c * 8), (&self.state.dye, c * 16), (&self.state.pres, c * 4)]
    }

    fn write_params(&self) {
        let keep = |secs: f32| if secs > 0.0 { (-self.dt / secs as f64).exp() as f32 } else { 1.0 };
        let p = GpuParams {
            width: self.cfg.width,
            height: self.cfg.height,
            dt: self.dt as f32,
            vel_keep: keep(self.cfg.velocity_fade_secs),
            dye_keep: keep(self.cfg.dye_fade_secs),
            curl: self.cfg.curl,
            sor: SOR,
            conserve: self.cfg.conserve_dye as u32,
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
            self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("vo-fluid") })
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
        self.dispatch_groups(enc, pipe, group1, ts, self.groups());
    }

    fn dispatch_groups(
        &self,
        enc: &mut wgpu::CommandEncoder,
        pipe: &wgpu::ComputePipeline,
        group1: Option<(&wgpu::BindGroup, &[u32])>,
        ts: Option<wgpu::ComputePassTimestampWrites<'_>>,
        (gx, gy): (u32, u32),
    ) {
        let mut pass =
            enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("vo-fluid"), timestamp_writes: ts });
        pass.set_pipeline(pipe);
        pass.set_bind_group(0, &self.bind, &[]);
        if let Some((bind, offsets)) = group1 {
            pass.set_bind_group(1, bind, offsets);
        }
        pass.dispatch_workgroups(gx, gy, 1);
    }

    /// Records one tick. `ts_first` goes on the first pass, `ts_last` on the last.
    fn record_tick(
        &self,
        ts_first: Option<wgpu::ComputePassTimestampWrites<'_>>,
        ts_last: Option<wgpu::ComputePassTimestampWrites<'_>>,
    ) {
        let p = &self.pipes;
        self.record(|enc| {
            self.dispatch(enc, &p.curl, None, ts_first);
            self.dispatch(enc, &p.confine, None, None);
            self.dispatch(enc, &p.advect_vel, None, None);
            self.dispatch(enc, &p.divergence, None, None);
            for _ in 0..self.cfg.iterations() {
                self.dispatch(enc, &p.sor_red, None, None);
                self.dispatch(enc, &p.sor_black, None, None);
            }
            self.dispatch(enc, &p.project, None, None);
            if self.cfg.conserve_dye {
                self.dispatch_groups(enc, &p.sum_before, None, None, (1, 1));
            }
            self.dispatch(enc, &p.advect_dye, None, None);
            if self.cfg.conserve_dye {
                self.dispatch_groups(enc, &p.sum_after, None, None, (1, 1));
            }
            self.dispatch(enc, &p.store_dye, None, ts_last);
        });
        self.pending.borrow_mut().dirty = true;
    }

    /// Submits the recorded work and, when the state changed, refreshes the dye and velocity
    /// textures. Call it after moving the timeline and before sampling the textures.
    pub fn flush(&self) {
        let dirty = self.pending.borrow().dirty;
        if dirty {
            self.record(|enc| self.dispatch(enc, &self.pipes.output, Some((&self.out_bind, &[])), None));
        }
        self.submit();
    }

    /// Writes the splat uniforms and submits the pending encoder, without the output pass.
    fn submit(&self) {
        let mut p = self.pending.borrow_mut();
        if !p.splats.is_empty() {
            let mut bytes = vec![0u8; SPLAT_STRIDE as usize * p.splats.len()];
            for (i, s) in p.splats.iter().enumerate() {
                let at = i * SPLAT_STRIDE as usize;
                bytes[at..at + size_of::<GpuSplat>()].copy_from_slice(bytemuck::bytes_of(s));
            }
            self.queue.write_buffer(&self.splat_buf, 0, &bytes);
            p.splats.clear();
        }
        if let Some(enc) = p.enc.take() {
            self.queue.submit([enc.finish()]);
        }
        p.dirty = false;
    }

    /// Reads the dye back, one premultiplied `[r, g, b, amount]` a cell, row by row. Waits for
    /// the GPU.
    pub fn read_dye(&self) -> Vec<[f32; 4]> {
        let bytes = self.read_buffer(&self.state.dye, self.cells() as u64 * 16);
        bytes.as_chunks::<16>().0.iter().map(|c| std::array::from_fn(|k| f32_at(c, k))).collect()
    }

    /// Reads the velocity back in grid cells per second, row by row. Waits for the GPU.
    pub fn read_velocity(&self) -> Vec<[f32; 2]> {
        let bytes = self.read_buffer(&self.state.vel, self.cells() as u64 * 8);
        bytes.as_chunks::<8>().0.iter().map(|c| [f32_at(c, 0), f32_at(c, 1)]).collect()
    }

    /// Reads the dye texture back as tightly packed `Rgba16Float` rows (8 bytes a cell).
    pub fn read_dye_texture(&self) -> Vec<u8> {
        self.read_texture(&self.dye_tex)
    }

    /// Reads the velocity texture back as tightly packed `Rgba16Float` rows (8 bytes a cell).
    pub fn read_velocity_texture(&self) -> Vec<u8> {
        self.read_texture(&self.vel_tex)
    }

    fn read_buffer(&self, src: &wgpu::Buffer, bytes: u64) -> Vec<u8> {
        self.flush();
        let rb = self.readback_buffer(bytes);
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_buffer_to_buffer(src, 0, &rb, 0, bytes);
        self.queue.submit([enc.finish()]);
        self.map(&rb)
    }

    fn read_texture(&self, tex: &wgpu::Texture) -> Vec<u8> {
        self.flush();
        let (w, h) = (self.cfg.width, self.cfg.height);
        let row = (w * 8).next_multiple_of(256);
        let rb = self.readback_buffer(row as u64 * h as u64);
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            tex.as_image_copy(),
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
            label: Some("vo-fluid-readback"),
            size,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn map(&self, buf: &wgpu::Buffer) -> Vec<u8> {
        buf.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map fluid readback"));
        self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: None }).expect("poll the GPU");
        let data = buf.slice(..).get_mapped_range().expect("read the mapped fluid buffer").to_vec();
        buf.unmap();
        data
    }

    /// Measures the GPU time of `ticks` ticks and one output pass on the current state, then
    /// puts the state back. `None` when the device has no `TIMESTAMP_QUERY`.
    pub fn gpu_cost(&mut self, ticks: u32) -> Option<FluidCost> {
        if !self.device.features().contains(wgpu::Features::TIMESTAMP_QUERY) || ticks == 0 {
            return None;
        }
        self.flush();
        let saved = self.snapshot();
        let set = self.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("vo-fluid-cost"),
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
        self.record(|enc| self.dispatch(enc, &self.pipes.output, Some((&self.out_bind, &[])), ts(Some(2), Some(3))));
        let resolve = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-fluid-cost"),
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
        Some(FluidCost {
            tick_ms: stamps[1].saturating_sub(stamps[0]) as f64 * period / ticks as f64,
            output_ms: stamps[3].saturating_sub(stamps[2]) as f64 * period,
        })
    }
}

fn f32_at(bytes: &[u8], k: usize) -> f32 {
    f32::from_le_bytes([bytes[4 * k], bytes[4 * k + 1], bytes[4 * k + 2], bytes[4 * k + 3]])
}

impl Sim for Fluid {
    type Event = FluidSplat;
    type Snapshot = FluidSnapshot;

    fn reset(&mut self) {
        self.record(|enc| {
            for (b, _) in self.state_parts() {
                enc.clear_buffer(b, 0, None);
            }
            for b in &self.scratch {
                enc.clear_buffer(b, 0, None);
            }
        });
        self.pending.borrow_mut().dirty = true;
    }

    fn step(&mut self, _tick: u64, dt: f64) {
        if self.dt != dt {
            // The params uniform is written at the next submit, so finish the work recorded
            // under the old one first.
            self.submit();
            self.pending.borrow_mut().dirty = true;
            self.dt = dt;
            self.write_params();
        }
        if self.cfg.reduced_motion {
            return;
        }
        self.record_tick(None, None);
    }

    fn apply(&mut self, e: &FluidSplat) {
        if self.pending.borrow().splats.len() == SPLAT_SLOTS {
            self.submit();
            self.pending.borrow_mut().dirty = true;
        }
        let (w, h) = (self.cfg.width as f32, self.cfg.height as f32);
        let push = if self.cfg.reduced_motion { [0.0; 2] } else { e.velocity };
        let [r, g, b] = e.color;
        let slot = {
            let mut p = self.pending.borrow_mut();
            p.splats.push(GpuSplat {
                x: e.x * w - 0.5,
                y: e.y * h - 0.5,
                radius: (e.radius * w).max(0.5),
                _pad: 0.0,
                vx: push[0] * w,
                vy: push[1] * h,
                _pad2: [0.0; 2],
                dye: [r * e.amount, g * e.amount, b * e.amount, e.amount],
            });
            p.dirty = true;
            p.splats.len() - 1
        };
        let offset = [(slot as u64 * SPLAT_STRIDE) as u32];
        self.record(|enc| self.dispatch(enc, &self.pipes.splat, Some((&self.splat_bind, &offset)), None));
    }

    fn snapshot(&mut self) -> FluidSnapshot {
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo-fluid-snapshot"),
            size: self.snapshot_size() as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.record(|enc| {
            let mut at = 0;
            for (b, n) in self.state_parts() {
                enc.copy_buffer_to_buffer(b, 0, &buf, at, n);
                at += n;
            }
        });
        FluidSnapshot { buf }
    }

    fn restore(&mut self, snap: &FluidSnapshot) {
        self.record(|enc| {
            let mut at = 0;
            for (b, n) in self.state_parts() {
                enc.copy_buffer_to_buffer(&snap.buf, at, b, 0, n);
                at += n;
            }
        });
        self.pending.borrow_mut().dirty = true;
    }

    fn snapshot_bytes(&self, _snap: &FluidSnapshot) -> usize {
        self.snapshot_size()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_follows_the_popup_size() {
        let c = FluidConfig::for_size(600.0, 300.0);
        assert_eq!((c.width, c.height), DEFAULT_GRID);
        let c = FluidConfig::for_size(900.0, 150.0);
        assert_eq!((c.width, c.height), (192, 32));
    }

    #[test]
    fn at_least_one_pressure_sweep() {
        let c = |n| FluidConfig { pressure_iterations: n, ..FluidConfig::default() }.iterations();
        assert_eq!((c(0), c(1), c(20)), (1, 1, 20));
    }

    #[test]
    fn splat_uniform_matches_the_shader() {
        // The WGSL `Splat` is 12 floats with `dye` at byte 32.
        assert_eq!(size_of::<GpuSplat>(), 48);
        assert_eq!(std::mem::offset_of!(GpuSplat, dye), 32);
        assert_eq!(size_of::<GpuParams>(), 32);
        assert_eq!(std::mem::offset_of!(GpuParams, conserve), 28);
    }
}
