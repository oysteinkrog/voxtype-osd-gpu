//! The noise library (FX10) on the GPU matches its Rust side: `noise.wgsl` and
//! `vo_core::noise` give the same values at the same points, the hashes bit for bit.

mod support;

use vo_core::noise;
use vo_render::renderer::noise_shader_source;
use wgpu::util::DeviceExt;

/// Values per point the compute shader writes.
const PER_POINT: usize = 12;

const ENTRY: &str = r#"
@group(0) @binding(0) var<storage, read> points: array<vec2f>;
@group(0) @binding(1) var<storage, read_write> out: array<f32>;

@compute @workgroup_size(64) fn main (@builtin(global_invocation_id) id: vec3u) {
  let i = id.x;
  if (i >= arrayLength (&points)) {
    return;
  }
  let p = points[i];
  let o = i * 12u;
  out[o] = bitcast<f32> (noise_rand (7u, i, 3u, 9u));
  out[o + 1u] = noise_ign (p * 37.0);
  out[o + 2u] = noise_value2 (p, 11u);
  let s = noise_simplex2_grad (p, 5u, 0.7);
  out[o + 3u] = s.x;
  out[o + 4u] = s.y;
  out[o + 5u] = s.z;
  out[o + 6u] = noise_fbm2 (p, 3u, 5, 0.2);
  let c = noise_curl2 (p, 13u, 1.1);
  out[o + 7u] = c.x;
  out[o + 8u] = c.y;
  let w = noise_warp2 (p, 17u, 0.75, 0.4);
  out[o + 9u] = w.x;
  out[o + 10u] = w.y;
  out[o + 11u] = noise_rand01 (2u, i, 0u, 0u);
}
"#;

fn points() -> Vec<[f32; 2]> {
    (0..1024).map(|i| [(i % 32) as f32 * 0.377 - 5.3, (i / 32) as f32 * 0.291 - 4.1]).collect()
}

fn cpu(i: u32, p: [f32; 2]) -> [f32; PER_POINT] {
    let s = noise::simplex2_grad(p, 5, 0.7);
    let c = noise::curl2(p, 13, 1.1);
    let w = noise::warp2(p, 17, 0.75, 0.4);
    [
        f32::from_bits(vo_core::hash::rand(7, i, 3, 9)),
        noise::ign([p[0] * 37.0, p[1] * 37.0]),
        noise::value2(p, 11),
        s[0],
        s[1],
        s[2],
        noise::fbm2(p, 3, 5, 0.2),
        c[0],
        c[1],
        w[0],
        w[1],
        vo_core::hash::rand_f32(2, i, 0, 0),
    ]
}

fn gpu_values(points: &[[f32; 2]]) -> Vec<f32> {
    let g = support::gpu();
    let (d, q) = (&g.device, &g.queue);
    let source = [noise_shader_source().into_owned(), ENTRY.to_string()].join("\n");
    let module = d.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("noise-test"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = d.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("noise-test"),
        layout: None,
        module: &module,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let input = d.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: None,
        contents: bytemuck::cast_slice(points),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let size = (points.len() * PER_POINT * 4) as u64;
    let output = d.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let rb = d.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bind = d.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry { binding: 0, resource: input.as_entire_binding() },
            wgpu::BindGroupEntry { binding: 1, resource: output.as_entire_binding() },
        ],
    });
    let mut enc = d.create_command_encoder(&Default::default());
    {
        let mut pass = enc.begin_compute_pass(&Default::default());
        pass.set_pipeline(&pipeline);
        pass.set_bind_group(0, &bind, &[]);
        pass.dispatch_workgroups((points.len() as u32).div_ceil(64), 1, 1);
    }
    enc.copy_buffer_to_buffer(&output, 0, &rb, 0, size);
    q.submit([enc.finish()]);
    rb.slice(..).map_async(wgpu::MapMode::Read, |_| {});
    d.poll(wgpu::PollType::wait_indefinitely()).unwrap();
    let data = rb.slice(..).get_mapped_range().expect("map");
    bytemuck::cast_slice(&data).to_vec()
}

#[test]
fn gpu_noise_matches_the_rust_side() {
    let points = points();
    let got = gpu_values(&points);
    let names =
        ["rand", "ign", "value2", "simplex2", "d/dx", "d/dy", "fbm2", "curl.x", "curl.y", "warp.x", "warp.y", "rand01"];
    let mut worst = [0.0f32; PER_POINT];
    for (i, p) in points.iter().enumerate() {
        let want = cpu(i as u32, *p);
        let have = &got[i * PER_POINT..(i + 1) * PER_POINT];
        // The hashes are integers: equal bits.
        assert_eq!(have[0].to_bits(), want[0].to_bits(), "rand at point {i}");
        assert_eq!(have[11], want[11], "rand01 at point {i}");
        for k in 1..PER_POINT {
            worst[k] = worst[k].max((have[k] - want[k]).abs());
        }
    }
    // Float rounding differs (fused multiply-add, cos and sin), so the rest match closely.
    // The curl is a derivative, ten times the value's scale.
    let limits = [0.0, 2e-3, 1e-5, 1e-4, 1e-3, 1e-3, 1e-4, 1e-3, 1e-3, 1e-4, 1e-4, 0.0];
    for k in 1..PER_POINT {
        assert!(worst[k] <= limits[k], "{}: GPU and CPU differ by {}", names[k], worst[k]);
    }
}
