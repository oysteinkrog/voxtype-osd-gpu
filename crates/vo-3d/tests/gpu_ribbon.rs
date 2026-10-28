//! Ribbons and cloth that twist and fold (EG-13): goldens at scale 1 and 1.5, on a light
//! background and under reduced motion, and the back face showing where the twist passes a
//! quarter turn. GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::f32::consts::{FRAC_PI_2, PI};

use support::{Image, Tolerance};
use vo_3d::*;
use vo_scene::emitter::LevelTrack;
use vo_scene::geom::{Color, Size};

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

/// MSAA resolve and transcendental maths differ a little between drivers.
const TOL: Tolerance = Tolerance { per_pixel: 3, max_fraction: 0.002 };

/// Draws `item` at `t` into a `w` x `h` logical target at `scale`, through the same path an
/// element takes: add the mesh once, rewrite its vertices, draw the frame.
fn draw(gpu: &support::Gpu, item: &RibbonItem, t: f64, w: u32, h: u32, scale: f32) -> (u32, u32, Vec<u8>) {
    let (pw, ph) = ((w as f32 * scale).round() as u32, (h as f32 * scale).round() as u32);
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let base = r.add_mesh(&item.meshes()[0]);
    let mut moving = Vec::new();
    item.vertices(t, &mut moving);
    for (i, v) in &moving {
        r.update_mesh(MeshId(base.0 + i), v);
    }
    let frame = item.frame(t, Size::new(w as f32, h as f32));
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("gpu_ribbon"),
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
    r.encode_with(&mut enc, Output { view: &view, size: [pw, ph], format: FORMAT, scale }, &frame, None, None);
    gpu.queue.submit([enc.finish()]);
    (pw, ph, readback::read_rgba(&gpu.device, &gpu.queue, &tex))
}

fn speech() -> LevelTrack {
    let mut l = LevelTrack::default();
    for (t, v) in [(0.0, 0.0), (0.4, 0.9), (0.9, 0.3), (1.3, 1.0), (2.0, 0.5)] {
        l.push(t, v);
    }
    l
}

fn silk() -> RibbonItem {
    RibbonItem { ribbon: Ribbon3d::default(), level: speech(), look: RibbonLook::default() }
}

#[test]
fn ribbon_goldens() {
    let gpu = support::gpu();
    let (w, h) = (300, 150);
    let mut results = Vec::new();
    let mut check = |name: &str, item: &RibbonItem, t: f64, scale: f32| {
        let (pw, ph, px) = draw(&gpu, item, t, w, h, scale);
        results.push(support::check(&Image { name: name.into(), w: pw, h: ph, channels: 4, px }, TOL));
    };
    let dark = RibbonItem {
        look: RibbonLook { background: Background::Solid(Color::hex(0x07090f, 1.0)), ..RibbonLook::default() },
        ..silk()
    };
    check("3d-ribbon", &dark, 1.6, 1.0);
    check("3d-ribbon-1.5x", &dark, 1.6, 1.5);
    let light = RibbonItem {
        look: RibbonLook {
            front: Color::hex(0x2a4bd8, 1.0),
            back: Color::hex(0xd84a7a, 1.0),
            background: Background::Solid(Color::hex(0xf2efe8, 1.0)),
            ..RibbonLook::default()
        },
        ..silk()
    };
    check("3d-ribbon-light", &light, 1.6, 1.0);
    let reduced = RibbonItem { ribbon: Ribbon3d { reduced_motion: true, ..Ribbon3d::default() }, ..dark.clone() };
    check("3d-ribbon-reduced", &reduced, 1.6, 1.0);
    let cloth = RibbonItem { ribbon: Ribbon3d::cloth(), ..dark };
    check("3d-cloth", &cloth, 1.6, 1.0);
    let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn back_shows_past_a_quarter_turn() {
    let gpu = support::gpu();
    let ribbon = Ribbon3d { twist: PI, ..Ribbon3d::default() };
    // The pose where the middle of the strip passes 90 degrees.
    let t = (0..2000)
        .map(|i| i as f64 * 0.005)
        .min_by(|a, b| {
            let off = |t: f64| (ribbon.twist_at(0.5, t) - FRAC_PI_2).abs();
            off(*a).total_cmp(&off(*b))
        })
        .unwrap();
    assert!((ribbon.twist_at(0.5, t) - FRAC_PI_2).abs() < 0.02);
    let look = RibbonLook {
        front: Color::hex(0xff0000, 1.0),
        back: Color::hex(0x0000ff, 1.0),
        fibre: 0.0,
        specular: 0.0,
        light: Light { rim: 0.0, ..Light::default() },
        ..RibbonLook::default()
    };
    let item = RibbonItem { ribbon, level: LevelTrack::default(), look };
    let (_, _, px) = draw(&gpu, &item, t, 300, 150, 1.0);
    let (mut ribbon_px, mut back_px) = (0, 0);
    for p in px.chunks_exact(4) {
        if p[3] < 128 {
            continue;
        }
        ribbon_px += 1;
        if p[2] > p[0] {
            back_px += 1;
        }
    }
    let share = back_px as f64 / ribbon_px.max(1) as f64;
    eprintln!("ribbon back share at the quarter turn: {back_px} of {ribbon_px} ({:.1} %)", share * 100.0);
    assert!(ribbon_px > 1000, "the ribbon covers only {ribbon_px} pixels");
    assert!(share >= 0.10, "back faces are {:.1} % of the ribbon", share * 100.0);
}

#[test]
fn update_mesh_redraws_the_same_pixels() {
    // Drawing t = 1 again after t = 2 gives the same pixels: the vertex rewrite holds no
    // state.
    let gpu = support::gpu();
    let item = silk();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let id = r.add_mesh(&item.meshes()[0]);
    let mut shot = |t: f64| {
        let mut moving = Vec::new();
        item.vertices(t, &mut moving);
        r.update_mesh(id, &moving[0].1);
        let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d { width: 160, height: 80, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = tex.create_view(&Default::default());
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.encode(&mut enc, &view, [160, 80], FORMAT, &item.frame(t, Size::new(160.0, 80.0)), None);
        gpu.queue.submit([enc.finish()]);
        readback::read_rgba(&gpu.device, &gpu.queue, &tex)
    };
    let a = shot(1.0);
    let b = shot(2.0);
    let c = shot(1.0);
    assert_ne!(a, b, "the ribbon did not move between t = 1 and t = 2");
    assert_eq!(a, c, "drawing t = 1 again changed the pixels");
}
