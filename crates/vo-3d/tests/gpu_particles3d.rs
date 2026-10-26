//! vo-render's 3D particles drawn inside the 3D pass through `Renderer3d::encode_with`: a
//! tilted torus emitter cutting through a lit cube, depth tested against the cube without
//! writing depth (D302 on the compute engine, D304). GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::particles::{
    tilt_x, Camera as PCamera, EmitterLook, EmitterSim, EmitterSpec, FrameParams, ParticleConfig, Particles,
};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Affine, Color};

const W: u32 = 320;
const H: u32 = 200;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[test]
fn particles_draw_in_the_depth_pass() {
    let gpu = support::gpu();
    let mut r = Renderer3d::new(&gpu.device, &gpu.queue);
    let cube = r.add_mesh(&Mesh::cuboid(Vec3::new(1.4, 1.4, 1.4)));
    let mut p = Particles::new(
        &gpu.device,
        &gpu.queue,
        FontRegistry::new(),
        ParticleConfig { free_capacity: 8192, word_slots: 1, slot_capacity: 64, ..Default::default() },
    );
    p.add_emitter(EmitterSpec { three_d: true, warmup: 1.0, ..EmitterSpec::new(8192, 9) });

    let t = 1.5;
    let frame = Frame3d {
        camera: Camera::orbit(Vec3::ZERO, 0.3, 0.35, 4.2),
        background: Background::Solid(Color::hex(0x0d1018, 1.0)),
        meshes: vec![MeshDraw {
            mesh: cube,
            model: Mat4::rotate_y(0.5),
            material: Material { color: Color::hex(0xff7a45, 1.0), ..Default::default() },
        }],
        ..Default::default()
    };
    p.advance(t, &|_, _| EmitterSim { swirl: 0.6, ..EmitterSim::torus(1.1, 0.08) });
    let mut params = FrameParams::new(t, Affine::IDENTITY, W, H, FORMAT);
    params.camera = Some(PCamera {
        view_proj: frame.camera.view_proj(W as f32 / H as f32).cols,
        focal_px: frame.camera.focal_px(H as f32),
    });
    params.depth = Some(render::DEPTH_FORMAT);
    params.samples = render::SAMPLES;
    let look = |_| EmitterLook {
        rotation: tilt_x(0.8),
        size: 0.012,
        brightness: 0.9,
        ramp: [Color::hex(0xa8e6ff, 1.0), Color::hex(0x6a8cff, 0.8), Color::hex(0x9b5cff, 0.0)],
        ..Default::default()
    };
    p.prepare(&params, &look, &[]);

    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: None,
        size: wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: FORMAT,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let view = tex.create_view(&Default::default());
    let draw = |r: &mut Renderer3d, frame: &Frame3d, with_particles: bool| {
        let mut enc = gpu.device.create_command_encoder(&Default::default());
        let out = Output { view: &view, size: [W, H], format: FORMAT, scale: 1.0 };
        let mut extra = |pass: &mut wgpu::RenderPass<'_>, info: &PassInfo| {
            assert_eq!((info.samples, info.depth), (render::SAMPLES, render::DEPTH_FORMAT));
            p.draw(pass);
        };
        r.encode_with(&mut enc, out, frame, None, with_particles.then_some(&mut extra as ExtraDraw<'_>));
        gpu.queue.submit([enc.finish()]);
        readback::read_rgba(&gpu.device, &gpu.queue, &tex)
    };
    let plain = draw(&mut r, &frame, false);
    let px = draw(&mut r, &frame, true);
    let empty = Frame3d { meshes: Vec::new(), ..frame.clone() };
    let ring_only = draw(&mut r, &empty, true);
    let background = draw(&mut r, &empty, false);

    let golden = support::check(
        &Image { name: "3d-particles-depth".into(), w: W, h: H, channels: 4, px: px.clone() },
        Tolerance { per_pixel: 3, max_fraction: 0.004 },
    );
    // The cube hides the ring's back arc and the part inside it, and the front arc still
    // shows: fewer particle pixels than with no cube, but far from none.
    let lit =
        |a: &[u8], b: &[u8]| a.as_chunks::<4>().0.iter().zip(b.as_chunks::<4>().0).filter(|(x, y)| x != y).count();
    let (with_cube, alone) = (lit(&plain, &px), lit(&background, &ring_only));
    assert!(alone > 2000, "the particles drew too little: {alone} pixels");
    assert!(with_cube < alone * 85 / 100, "the cube hid too little: {with_cube} of {alone} pixels");
    assert!(with_cube > alone * 30 / 100, "the cube hid too much: {with_cube} of {alone} pixels");
    golden.unwrap();
}
