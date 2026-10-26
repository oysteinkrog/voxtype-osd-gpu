//! GPU time per stage from timestamp queries written between passes. Needs the device
//! features `TIMESTAMP_QUERY` and `TIMESTAMP_QUERY_INSIDE_ENCODERS`; [`GpuTimer::new`] gives
//! `None` without them, and callers then skip timing.

const CAPACITY: u32 = 32;

pub struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    period_ns: f32,
    labels: Vec<&'static str>,
}

/// Timestamp query features a device needs for [`GpuTimer`].
pub const FEATURES: wgpu::Features =
    wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS);

impl GpuTimer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<GpuTimer> {
        if !device.features().contains(FEATURES) {
            return None;
        }
        let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
            label: Some("vo3d-timer"),
            ty: wgpu::QueryType::Timestamp,
            count: CAPACITY,
        });
        let bytes = CAPACITY as u64 * 8;
        let resolve = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo3d-timer-resolve"),
            size: bytes,
            usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("vo3d-timer-read"),
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Some(GpuTimer { queries, resolve, read, period_ns: queue.get_timestamp_period(), labels: Vec::new() })
    }

    /// Starts a frame: forgets the previous marks.
    pub fn begin(&mut self) {
        self.labels.clear();
    }

    /// Writes a timestamp into `enc`. The time between two marks is reported under the
    /// later mark's label. Marks past the capacity are dropped.
    pub fn mark(&mut self, enc: &mut wgpu::CommandEncoder, label: &'static str) {
        if self.labels.len() as u32 >= CAPACITY {
            return;
        }
        enc.write_timestamp(&self.queries, self.labels.len() as u32);
        self.labels.push(label);
    }

    /// Resolves the marks, waits for the GPU, and returns `(label, milliseconds)` for each
    /// span between consecutive marks.
    pub fn read(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) -> Vec<(&'static str, f64)> {
        let n = self.labels.len() as u32;
        if n < 2 {
            return Vec::new();
        }
        let mut enc = device.create_command_encoder(&Default::default());
        enc.resolve_query_set(&self.queries, 0..n, &self.resolve, 0);
        enc.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, n as u64 * 8);
        queue.submit([enc.finish()]);
        self.read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let ticks: Vec<u64> = {
            let data = self.read.slice(..).get_mapped_range().expect("mapped");
            bytemuck::cast_slice::<u8, u64>(&data[..n as usize * 8]).to_vec()
        };
        self.read.unmap();
        let ms = |a: u64, b: u64| b.wrapping_sub(a) as f64 * self.period_ns as f64 * 1e-6;
        (1..n as usize).map(|i| (self.labels[i], ms(ticks[i - 1], ticks[i]))).collect()
    }
}
