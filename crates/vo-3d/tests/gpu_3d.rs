//! Golden images of the 3D layer: lit meshes with depth (D301), point clouds (D302), bokeh
//! (FX16). GPU test: `just test-gpu` runs it.
//!
//! Goldens are RGBA PNGs of the premultiplied bytes read back from an sRGB target, like
//! vo-render's. The contact sheet is written to /var/tmp/claude/vo-golden/.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::f32::consts::{FRAC_PI_2, PI};

use support::{Image, Tolerance};
use vo_3d::text3d::extrude_text;
use vo_3d::*;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::Color;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

fn target(gpu: &support::Gpu, w: u32, h: u32) -> wgpu::Texture {
    gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_3d"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    })
}

fn draw(gpu: &support::Gpu, r: &mut Renderer3d, frame: &Frame3d, w: u32, h: u32) -> Vec<u8> {
    let tex = target(gpu, w, h);
    let view = tex.create_view(&Default::default());
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    r.encode(&mut enc, &view, [w, h], FORMAT, frame, None);
    gpu.queue.submit([enc.finish()]);
    readback::read_rgba(&gpu.device, &gpu.queue, &tex)
}

fn image(name: &str, w: u32, h: u32, px: Vec<u8>) -> Image {
    Image { name: name.into(), w, h, channels: 4, px }
}

fn report(results: Vec<Result<(), String>>) {
    let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

/// MSAA resolve and transcendental maths differ a little between drivers.
const TOL: Tolerance = Tolerance { per_pixel: 3, max_fraction: 0.002 };

fn mesh_frame(r: &mut Renderer3d) -> Frame3d {
    let torus = r.add_mesh(&Mesh::torus(1.0, 0.3, 64, 24));
    let cube = r.add_mesh(&Mesh::cuboid(Vec3::new(0.7, 0.7, 0.7)));
    let fonts = FontRegistry::new();
    let face = fonts.get(fonts.add_file(&test_font_path()).unwrap()).unwrap();
    let mut meshes = vec![
        MeshDraw {
            mesh: torus,
            model: Mat4::rotate_x(0.5),
            material: Material { color: Color::hex(0x4f8cff, 1.0), ..Default::default() },
        },
        // The cube passes through the torus hole, so depth decides what shows.
        MeshDraw {
            mesh: cube,
            model: Mat4::rotate_y(0.6).then(&Mat4::rotate_x(0.4)).then(&Mat4::translation(Vec3::new(0.9, 0.0, 0.0))),
            material: Material { color: Color::hex(0xff7a45, 1.0), ..Default::default() },
        },
    ];
    for g in extrude_text(&face, "Vo", 0.9, 0.2) {
        let id = r.add_mesh(&g.mesh);
        meshes.push(MeshDraw {
            mesh: id,
            model: Mat4::translation(g.center + Vec3::new(-1.0, 1.05, 0.2)),
            material: Material { color: Color::hex(0xf2f2f2, 1.0), emissive: 0.1, ..Default::default() },
        });
    }
    Frame3d {
        camera: Camera::orbit(Vec3::new(0.0, 0.3, 0.0), 0.35, 0.3, 5.0),
        background: Background::Vertical { top: Color::hex(0x10131a, 1.0), bottom: Color::hex(0x272c38, 1.0) },
        meshes,
        ..Default::default()
    }
}

#[test]
fn lit_meshes_with_depth() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let frame = mesh_frame(&mut r);
    let (w, h) = (320, 200);
    let mut results = vec![support::check(&image("3d-meshes", w, h, draw(&gpu, &mut r, &frame, w, h)), TOL)];

    // Orthographic and transparent: the default 2D case of the same layer.
    let mut ortho = frame.clone();
    ortho.camera.projection = Projection::Orthographic { height: 3.2 };
    ortho.background = Background::Transparent;
    results.push(support::check(&image("3d-meshes-ortho", w, h, draw(&gpu, &mut r, &ortho, w, h)), TOL));
    report(results);
}

#[test]
fn same_t_same_pixels() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let mut frame = mesh_frame(&mut r);
    frame.clouds.push(ring(2.0, 0.4));
    let a = draw(&gpu, &mut r, &frame, 160, 100);
    let mut other = frame.clone();
    other.clouds[0].t = 3.0;
    let _ = draw(&gpu, &mut r, &other, 160, 100);
    let b = draw(&gpu, &mut r, &frame, 160, 100);
    assert_eq!(a, b, "drawing t = 2 again after t = 3 changed the pixels");
}

/// The voice ring: a tilted torus of points.
fn ring(t: f32, level: f32) -> PointCloud {
    PointCloud {
        shape: CloudShape::Torus { major: 1.2, minor: 0.12 },
        model: Mat4::rotate_x(FRAC_PI_2 - 0.45).then(&Mat4::rotate_z(0.15)),
        count: 6000,
        seed: 7,
        color: Color::hex(0x7fd4ff, 0.9),
        color2: Color::hex(0xb48cff, 0.9),
        size: 0.018,
        min_px: 1.2,
        t,
        speed: 0.35,
        level,
        level_amp: 2.5,
        ..Default::default()
    }
}

#[test]
fn point_ring_reacts_to_level() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let (w, h) = (320, 200);
    let mut results = Vec::new();
    let mut lit = Vec::new();
    for (name, level) in [("3d-ring-quiet", 0.0), ("3d-ring-loud", 1.0)] {
        let frame = Frame3d {
            camera: Camera::orbit(Vec3::ZERO, 0.0, 0.25, 4.2),
            clouds: vec![ring(1.5, level)],
            fog: Some((3.0, 6.5)),
            ..Default::default()
        };
        let px = draw(&gpu, &mut r, &frame, w, h);
        lit.push(spread(&px, w, h));
        results.push(support::check(&image(name, w, h, px), TOL));
    }
    // A loud ring spreads wider than a quiet one.
    assert!(lit[1] > lit[0] * 1.05, "level did not widen the ring: {lit:?}");
    report(results);
}

/// Mean distance of lit pixels from the image centre, weighted by alpha.
fn spread(px: &[u8], w: u32, h: u32) -> f32 {
    let (mut sum, mut weight) = (0.0f64, 0.0f64);
    for (i, p) in px.as_chunks::<4>().0.iter().enumerate() {
        let (x, y) = ((i as u32 % w) as f64 - w as f64 / 2.0, (i as u32 / w) as f64 - h as f64 / 2.0);
        let a = p[3] as f64;
        sum += a * (x * x + y * y).sqrt();
        weight += a;
    }
    (sum / weight.max(1.0)) as f32
}

#[test]
fn bokeh_orbs_blur_with_depth() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let (w, h) = (320, 200);
    // Five orbs from near to far; the focus plane is at the middle one.
    let orbs = (0..5)
        .map(|i| {
            let k = i as f32 / 4.0;
            Orb {
                pos: Vec3::new(-1.6 + 0.8 * i as f32, 0.15 * (k * PI * 2.0).sin(), 1.8 - 3.6 * k),
                radius: 0.06,
                color: Color::hex(if i % 2 == 0 { 0xffc46b } else { 0x6bd7ff }, 1.0),
                intensity: 1.6,
            }
        })
        .collect();
    let frame = Frame3d {
        camera: Camera::orbit(Vec3::ZERO, 0.0, 0.0, 4.0),
        background: Background::Solid(Color::hex(0x0b0d12, 1.0)),
        orbs,
        focus: Focus { distance: 4.0, range: 2.0, max_blur_px: 70.0 },
        ..Default::default()
    };
    let px = draw(&gpu, &mut r, &frame, w, h);
    report(vec![support::check(&image("3d-bokeh", w, h, px), TOL)]);
}

/// D305: a lit ring above a glossy floor, with a pool of light and a cone. The floor changes
/// nothing above its horizon and adds the mirrored ring and the pool below it.
#[test]
fn floor_reflection_and_light_cone() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let torus = r.add_mesh(&Mesh::torus(0.8, 0.16, 64, 20));
    let (w, h) = (320u32, 240u32);
    let ring = Mat4::rotate_x(FRAC_PI_2).then(&Mat4::translation(Vec3::new(0.0, 0.15, 0.0)));
    let base = Frame3d {
        camera: Camera::orbit(Vec3::new(0.0, -0.3, 0.0), 0.3, 0.3, 4.2),
        background: Background::Solid(Color::hex(0x0b0e16, 1.0)),
        meshes: vec![MeshDraw {
            mesh: torus,
            model: ring,
            material: Material { color: Color::hex(0x9fc8ff, 1.0), emissive: 0.4, ..Default::default() },
        }],
        clouds: vec![PointCloud {
            shape: CloudShape::Torus { major: 0.8, minor: 0.3 },
            model: ring,
            count: 3000,
            seed: 9,
            color: Color::hex(0x76c6ff, 0.8),
            color2: Color::hex(0xa98bff, 0.8),
            size: 0.02,
            ..Default::default()
        }],
        orbs: vec![Orb {
            pos: Vec3::new(1.3, 0.5, -0.6),
            radius: 0.06,
            color: Color::hex(0xffc477, 1.0),
            intensity: 2.0,
        }],
        ..Default::default()
    };
    let floor = Floor {
        y: -0.9,
        reflect: 0.5,
        fade: 1.0,
        pools: [
            Some(Pool { center: Vec3::ZERO, radius: 1.4, color: Color::hex(0x5f8dff, 1.0), intensity: 0.6 }),
            None,
            None,
            None,
        ],
    };
    let plain = draw(&gpu, &mut r, &base, w, h);
    let with_floor = draw(&gpu, &mut r, &Frame3d { floor: Some(floor), ..base.clone() }, w, h);
    let row = (w * 4) as usize;
    // The floor's horizon is below the top quarter at this pitch: nothing changes there.
    assert_eq!(plain[..row * (h as usize / 4)], with_floor[..row * (h as usize / 4)], "floor changed the sky");
    let light = |px: &[u8], y0: u32, y1: u32| -> u64 {
        px[row * y0 as usize..row * y1 as usize].chunks(4).map(|p| p[0] as u64 + p[1] as u64 + p[2] as u64).sum()
    };
    let (a, b) = (light(&plain, h * 3 / 4, h), light(&with_floor, h * 3 / 4, h));
    assert!(b > a + a / 4, "no reflection or pool below the ring: {a} -> {b}");

    let cone = LightCone {
        apex: Vec3::new(-1.0, 1.9, -0.6),
        dir: Vec3::new(0.3, -1.0, 0.2),
        half_angle: 0.35,
        length: 3.2,
        color: Color::hex(0x7aa8ff, 1.0),
        intensity: 0.6,
    };
    let full = draw(&gpu, &mut r, &Frame3d { floor: Some(floor), cones: vec![cone], ..base.clone() }, w, h);
    assert!(light(&full, 0, h / 4) > light(&plain, 0, h / 4), "the cone adds light near its apex");
    let same = draw(&gpu, &mut r, &Frame3d { floor: Some(floor), cones: vec![cone], ..base.clone() }, w, h);
    assert_eq!(full, same, "same frame, same pixels");
    report(vec![support::check(&image("3d-floor", w, h, full), TOL)]);
}
