//! Soft shadows from the key light (`vo_3d::Shadows`, EG-18): a box that bobs over the floor
//! and a turning torus, both casting onto the floor and onto each other.
//!
//!   cargo run -p vo-3d --example shadow_demo -- [--out DIR]
//!   cargo run -p vo-3d --example shadow_demo -- --bench
//!
//! The default writes `DIR/shadow_demo.png` (default DIR `/var/tmp/claude/fw-shadow`): one
//! row each for no shadows, the default shadows, wide soft shadows and the default on a light
//! floor, at six moments. `--bench` prints the GPU time per frame (timestamp queries) at
//! 600x300 with and without shadows; without timestamp queries it says so and exits 0.

use std::path::PathBuf;

use vo_3d::*;
use vo_render::offscreen::write_png;
use vo_scene::geom::Color;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Scene {
    cube: MeshId,
    torus: MeshId,
}

impl Scene {
    fn new(r: &mut Renderer3d) -> Scene {
        Scene {
            cube: r.add_mesh(&Mesh::cuboid(Vec3::new(0.6, 0.6, 0.6))),
            torus: r.add_mesh(&Mesh::torus(0.45, 0.14, 64, 20)),
        }
    }

    /// The frame at `t`: a pure function of `t`, so any moment draws the same.
    fn frame(&self, t: f32, shadows: Option<Shadows>, light_floor: bool) -> Frame3d {
        let bg = if light_floor { Color::hex(0xe9e4da, 1.0) } else { Color::hex(0x161a24, 1.0) };
        let lift = 0.3 + 0.3 * (t * 1.4).sin();
        Frame3d {
            camera: Camera::orbit(Vec3::new(0.0, -0.2, 0.0), 0.25, 0.45, 4.2),
            background: Background::Solid(bg),
            light: Light { dir: Vec3::new(0.55, -1.0, 0.35), ..Default::default() },
            meshes: vec![
                MeshDraw {
                    mesh: self.cube,
                    model: Mat4::rotate_y(0.5 + 0.4 * t).then(&Mat4::translation(Vec3::new(-0.3, lift, -0.2))),
                    material: Material { color: Color::hex(0xff7a45, 1.0), ..Default::default() },
                },
                MeshDraw {
                    mesh: self.torus,
                    model: Mat4::rotate_x(0.35 + 0.5 * (t * 0.8).sin())
                        .then(&Mat4::translation(Vec3::new(0.6, -0.25, 0.1))),
                    material: Material { color: Color::hex(0x4f8cff, 1.0), ..Default::default() },
                },
            ],
            floor: Some(Floor { y: -0.6, reflect: 0.25, fade: 0.6, pools: [None; MAX_POOLS] }),
            shadows,
            ..Default::default()
        }
    }
}

fn rows() -> Vec<(&'static str, Option<Shadows>, bool)> {
    vec![
        ("none", None, false),
        ("default", Some(Shadows::default()), false),
        ("soft", Some(Shadows { softness: 8.0, ..Shadows::default() }), false),
        ("light", Some(Shadows::default()), true),
    ]
}

fn gpu(timestamps: bool) -> Option<(wgpu::Device, wgpu::Queue)> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a Vulkan adapter");
    let features = if timestamps { vo_3d::timer::FEATURES } else { wgpu::Features::empty() };
    if !adapter.features().contains(features) {
        return None;
    }
    Some(
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("shadow-demo"),
            required_features: features,
            ..Default::default()
        }))
        .expect("a GPU device"),
    )
}

fn target(device: &wgpu::Device, w: u32, h: u32) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("shadow-demo"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn bench() {
    let Some((device, queue)) = gpu(true) else {
        println!("shadow_demo bench: this GPU has no timestamp queries; no timing");
        return;
    };
    let period = queue.get_timestamp_period() as f64;
    let mut r = Renderer3d::new(&device, &queue);
    let scene = Scene::new(&mut r);
    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let buf = |usage| {
        device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
    };
    let resolve = buf(wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC);
    let read = buf(wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let (w, h) = (600u32, 300u32);
    let tex = target(&device, w, h);
    let view = tex.create_view(&Default::default());
    let mut p95s = Vec::new();
    for (name, shadows, light) in rows() {
        let mut times = Vec::new();
        for i in 0..240 {
            let frame = scene.frame(1.0 + i as f32 / 60.0, shadows, light);
            let mut enc = device.create_command_encoder(&Default::default());
            enc.write_timestamp(&qs, 0);
            r.encode(&mut enc, &view, [w, h], FORMAT, &frame, None);
            enc.write_timestamp(&qs, 1);
            enc.resolve_query_set(&qs, 0..2, &resolve, 0);
            enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
            queue.submit([enc.finish()]);
            read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
            device.poll(wgpu::PollType::wait_indefinitely()).unwrap();
            let ts: [u64; 2] = bytemuck::pod_read_unaligned(&read.slice(..).get_mapped_range().unwrap()[..16]);
            read.unmap();
            if i >= 40 {
                times.push((ts[1] - ts[0]) as f64 * period / 1e6);
            }
        }
        times.sort_by(f64::total_cmp);
        let (median, p95) = (times[times.len() / 2], times[times.len() * 95 / 100]);
        p95s.push(p95);
        println!("shadow_demo bench: {name:7} {w}x{h}: {median:.3} ms median, {p95:.3} ms p95");
    }
    println!("shadow_demo bench: shadows add {:.3} ms p95 (default against none)", p95s[1] - p95s[0]);
}

fn main() {
    let mut args = std::env::args().skip(1);
    let mut out = PathBuf::from("/var/tmp/claude/fw-shadow");
    while let Some(a) = args.next() {
        match a.as_str() {
            "--out" => out = args.next().expect("--out needs a directory").into(),
            "--bench" => return bench(),
            _ => panic!("unknown option {a}"),
        }
    }
    let (w, h) = (300u32, 180u32);
    let (device, queue) = gpu(false).expect("a GPU device");
    let mut r = Renderer3d::new(&device, &queue);
    let scene = Scene::new(&mut r);
    let tex = target(&device, w, h);
    let view = tex.create_view(&Default::default());
    let rows = rows();
    let (cols, gap) = (6u32, 6u32);
    let (sw, sh) = (cols * w + (cols + 1) * gap, rows.len() as u32 * h + (rows.len() as u32 + 1) * gap);
    let mut sheet = vec![0u8; (sw * sh * 4) as usize];
    for (row, (_, shadows, light)) in rows.into_iter().enumerate() {
        for c in 0..cols {
            let frame = scene.frame(0.4 + 0.7 * c as f32, shadows, light);
            let mut enc = device.create_command_encoder(&Default::default());
            r.encode(&mut enc, &view, [w, h], FORMAT, &frame, None);
            queue.submit([enc.finish()]);
            let rgba = readback::read_rgba(&device, &queue, &tex);
            let (x0, y0) = (gap + c * (w + gap), gap + row as u32 * (h + gap));
            for y in 0..h {
                let s = (y * w * 4) as usize;
                let d = (((y0 + y) * sw + x0) * 4) as usize;
                sheet[d..d + (w * 4) as usize].copy_from_slice(&rgba[s..s + (w * 4) as usize]);
            }
        }
    }
    std::fs::create_dir_all(&out).unwrap();
    let path = out.join("shadow_demo.png");
    write_png(&path, sw, sh, &sheet).unwrap();
    println!("shadow_demo: wrote {}", path.display());
}
