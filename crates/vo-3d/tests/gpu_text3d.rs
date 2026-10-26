//! Extruded 3D text (D306, TY01 outlines) turning in letter by letter on springs. GPU test:
//! `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_core::spring::Spring;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::Color;

const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const TOL: Tolerance = Tolerance { per_pixel: 3, max_fraction: 0.002 };

#[test]
fn word_turns_in_on_springs() {
    let gpu = support::gpu();
    let fonts = FontRegistry::new();
    let face = fonts.get(fonts.add_file(&test_font_path()).unwrap()).unwrap();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let glyphs = extrude_text(&face, "Hello", 1.0, 0.25);
    let mut word = TurningWord::new(&mut r, &glyphs, 0.0, Spring::POP);
    word.turn(0.2, 1.0, 0.06, 0.3);

    let (w, h) = (240, 120);
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    let material = Material { color: Color::hex(0xe8ecf4, 1.0), specular: 0.6, ..Default::default() };
    let mut results = Vec::new();
    let mut lit = Vec::new();
    for (name, t) in [("3d-word-0.30", 0.30), ("3d-word-0.45", 0.45), ("3d-word-settled", 2.0)] {
        let frame = Frame3d {
            camera: Camera::orbit(Vec3::ZERO, 0.25, 0.12, 4.2),
            background: Background::Vertical { top: Color::hex(0x141821, 1.0), bottom: Color::hex(0x252b38, 1.0) },
            meshes: word.draws(t, &Mat4::IDENTITY, material),
            ..Default::default()
        };
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        r.encode(&mut enc, &view, [w, h], FORMAT, &frame, None);
        gpu.queue.submit([enc.finish()]);
        let px = readback::read_rgba(&gpu.device, &gpu.queue, &tex);
        lit.push(px.as_chunks::<4>().0.iter().filter(|p| p[0] > 120).count());
        results.push(support::check(&Image { name: name.into(), w, h, channels: 4, px }, TOL));
    }
    assert!(word.is_settled(2.0));
    // More letter faces turn toward the light as the word comes in.
    assert!(lit[0] < lit[1] && lit[1] < lit[2], "lit pixels {lit:?}");
    let errors: Vec<String> = results.into_iter().filter_map(Result::err).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
