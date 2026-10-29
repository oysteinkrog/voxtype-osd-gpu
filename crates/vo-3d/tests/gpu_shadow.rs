//! Soft shadows from the key light (EG-18): a box and a torus over the floor, at scale 1 and
//! 1.5 and on a light floor; the darkening and the soft edge measured on a scanline; and no
//! shadow map at all while a frame asks for none. GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_scene::geom::Color;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The noise-turned Poisson disc and MSAA differ a little between drivers.
const TOL: Tolerance = Tolerance { per_pixel: 3, max_fraction: 0.004 };
const FLOOR_Y: f32 = -0.6;

fn draw(gpu: &support::Gpu, r: &mut Renderer3d, frame: &Frame3d, w: u32, h: u32, scale: f32) -> Vec<u8> {
    let (pw, ph) = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_shadow"),
        size: wgpu::Extent3d { width: pw, height: ph, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    r.encode_with(&mut enc, Output { view: &view, size: [pw, ph], format: FORMAT, scale }, frame, None, None);
    gpu.queue.submit([enc.finish()]);
    readback::read_rgba(&gpu.device, &gpu.queue, &tex)
}

/// A box and a torus over a floor, lit from the upper left front.
fn scene(r: &mut Renderer3d, light_floor: bool) -> Frame3d {
    let cube = r.add_mesh(&Mesh::cuboid(Vec3::new(0.6, 0.6, 0.6)));
    let torus = r.add_mesh(&Mesh::torus(0.45, 0.14, 64, 20));
    let bg = if light_floor { Color::hex(0xe9e4da, 1.0) } else { Color::hex(0x161a24, 1.0) };
    Frame3d {
        camera: Camera::orbit(Vec3::new(0.0, -0.2, 0.0), 0.25, 0.45, 4.2),
        background: Background::Solid(bg),
        light: Light { dir: Vec3::new(0.55, -1.0, 0.35), ..Default::default() },
        meshes: vec![
            MeshDraw {
                mesh: cube,
                model: Mat4::rotate_y(0.5).then(&Mat4::translation(Vec3::new(-0.3, 0.35, -0.2))),
                material: Material { color: Color::hex(0xff7a45, 1.0), ..Default::default() },
            },
            MeshDraw {
                mesh: torus,
                model: Mat4::rotate_x(0.35).then(&Mat4::translation(Vec3::new(0.6, -0.25, 0.1))),
                material: Material { color: Color::hex(0x4f8cff, 1.0), ..Default::default() },
            },
        ],
        floor: Some(Floor { y: FLOOR_Y, reflect: 0.25, fade: 0.6, pools: [None; MAX_POOLS] }),
        shadows: Some(Shadows::default()),
        ..Default::default()
    }
}

fn image(name: &str, w: u32, h: u32, px: Vec<u8>) -> Image {
    Image { name: name.into(), w, h, channels: 4, px }
}

#[test]
fn shadow_goldens() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let dark = scene(&mut r, false);
    let light = scene(&mut r, true);
    let (w, h) = (320, 200);
    let a = draw(&gpu, &mut r, &dark, w, h, 1.0);
    let again = draw(&gpu, &mut r, &dark, w, h, 1.0);
    assert_eq!(a, again, "same frame, same pixels");
    let results = vec![
        support::check(&image("3d-shadow", w, h, a), TOL),
        support::check(&image("3d-shadow-1.5x", 480, 300, draw(&gpu, &mut r, &dark, w, h, 1.5)), TOL),
        support::check(&image("3d-shadow-light", w, h, draw(&gpu, &mut r, &light, w, h, 1.0)), TOL),
    ];
    let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));

    // Meshes receive: with no floor, shadows can only darken the meshes themselves.
    let bare = Frame3d { floor: None, ..dark.clone() };
    let lit = draw(&gpu, &mut r, &Frame3d { shadows: None, ..bare.clone() }, w, h, 1.0);
    let shaded = draw(&gpu, &mut r, &bare, w, h, 1.0);
    let darker = lit
        .chunks(4)
        .zip(shaded.chunks(4))
        .filter(|(a, b)| (a[0] as i32 + a[1] as i32 + a[2] as i32) - (b[0] as i32 + b[1] as i32 + b[2] as i32) >= 30)
        .count();
    eprintln!("meshes: {darker} pixels in shadow");
    assert!(darker >= 300, "no shadow on the meshes: {darker} pixels darker");
}

#[test]
fn no_shadows_no_shadow_map() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let mut frame = scene(&mut r, false);
    frame.shadows = None;
    let _ = draw(&gpu, &mut r, &frame, 160, 100, 1.0);
    assert_eq!(r.shadow_map_size(), None, "a frame without shadows made a shadow map");
    frame.shadows = Some(Shadows { size: 256, ..Shadows::default() });
    let _ = draw(&gpu, &mut r, &frame, 160, 100, 1.0);
    assert_eq!(r.shadow_map_size(), Some(256));
}

/// Screen pixel of world point `p` on a `w` x `h` target.
fn pixel(cam: &Camera, p: Vec3, w: u32, h: u32) -> (i32, i32) {
    let c = cam.view_proj(w as f32 / h as f32).transform_point(p);
    (((c[0] / c[3] + 1.0) * 0.5 * w as f32) as i32, ((1.0 - c[1] / c[3]) * 0.5 * h as f32) as i32)
}

/// Acceptance: a box 40 px above the floor darkens the floor under it by at least 30 %,
/// and the edge runs from 10 % to 90 % of that darkening over at least 3 px.
#[test]
fn floor_under_a_box_is_darker_with_a_soft_edge() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let cube = r.add_mesh(&Mesh::cuboid(Vec3::new(0.6, 0.6, 0.6)));
    let (w, h) = (600u32, 300u32);
    // Orthographic, 100 px per world unit, the floor seen from straight above: the box's
    // underside is 0.4 above the floor, 40 px.
    let camera = Camera {
        projection: Projection::Orthographic { height: 3.0 },
        ..Camera::orbit(Vec3::new(0.0, FLOOR_Y, 0.0), 0.0, 1.5, 4.0)
    };
    let dir = Vec3::new(0.5, -1.0, 0.0);
    let centre = Vec3::new(-0.9, FLOOR_Y + 0.7, 0.0);
    let frame = Frame3d {
        camera,
        background: Background::Solid(Color::hex(0xd8d4cc, 1.0)),
        light: Light { dir, ..Default::default() },
        meshes: vec![MeshDraw {
            mesh: cube,
            model: Mat4::translation(centre),
            material: Material { color: Color::hex(0x8a8f99, 1.0), ..Default::default() },
        }],
        floor: Some(Floor { y: FLOOR_Y, reflect: 0.0, fade: 1.0, pools: [None; MAX_POOLS] }),
        shadows: Some(Shadows::default()),
        ..Default::default()
    };
    let px = draw(&gpu, &mut r, &frame, w, h, 1.0);
    let lum = |x: i32, y: i32| {
        let i = ((y as u32 * w + x as u32) * 4) as usize;
        px[i] as f32 * 0.2126 + px[i + 1] as f32 * 0.7152 + px[i + 2] as f32 * 0.0722
    };
    // The shadow's centre: the box centre carried along the light down to the floor.
    let under = centre + dir.normalize() * ((centre.y - FLOOR_Y) / dir.normalize().y.abs());
    let (sx, sy) = pixel(&frame.camera, under, w, h);
    let open = lum(w as i32 - 10, sy);
    let shadowed = lum(sx, sy);
    let dark = 1.0 - shadowed / open;
    assert!(dark >= 0.3, "the floor under the box is only {:.0} % darker ({shadowed} vs {open})", dark * 100.0);
    // Walk right from the centre, away from the box, to the open floor.
    let mut x90 = None;
    let mut x10 = None;
    for x in sx..w as i32 - 10 {
        let d = 1.0 - lum(x, sy) / open;
        if x90.is_none() && d < 0.9 * dark {
            x90 = Some(x);
        }
        if d < 0.1 * dark {
            x10 = Some(x);
            break;
        }
    }
    let (x90, x10) = (x90.expect("no edge"), x10.expect("no open floor"));
    eprintln!("shadow: {:.0} % darker, edge {} px ({x90} to {x10})", dark * 100.0, x10 - x90);
    assert!(x10 - x90 >= 3, "the edge is only {} px wide ({x90} to {x10})", x10 - x90);
}
