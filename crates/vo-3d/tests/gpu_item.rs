//! 3D as an item in a 2D scene: a ring and a hoop drawn at a rect over a surface that already
//! holds content, with the layer opacity. GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::Target;
use vo_scene::geom::{Color, Size};

const W: u32 = 320;
const H: u32 = 200;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[derive(Debug)]
struct Ring;

impl Source3d for Ring {
    fn meshes(&self) -> Vec<Mesh> {
        vec![Mesh::torus(1.2, 0.03, 96, 12)]
    }

    fn frame(&self, t: f64, size: Size) -> Frame3d {
        assert_eq!(size, Size::new(200.0, 120.0));
        let tilt = Mat4::rotate_x(0.62);
        Frame3d {
            camera: Camera::orbit(Vec3::ZERO, 0.0, 0.1, 4.0),
            meshes: vec![MeshDraw {
                mesh: MeshId(0),
                model: tilt,
                material: Material { color: Color::hex(0x9fc8ff, 1.0), emissive: 0.5, ..Default::default() },
            }],
            clouds: vec![PointCloud {
                shape: CloudShape::Torus { major: 1.2, minor: 0.12 },
                model: tilt,
                count: 5000,
                seed: 4,
                color: Color::hex(0x76c6ff, 0.9),
                color2: Color::hex(0xa98bff, 0.9),
                size: 0.02,
                t: t as f32,
                ..Default::default()
            }],
            ..Default::default()
        }
    }
}

#[test]
fn item_composites_at_its_rect() {
    let gpu = support::gpu();
    let target = Target::new(&gpu.device, W, H, FORMAT);
    let view = target.texture().create_view(&Default::default());
    let mut painter = ItemPainter::new(&gpu.device, &gpu.queue);
    let mut enc = gpu.device.create_command_encoder(&Default::default());
    // The layers below: a solid backdrop.
    enc.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: None,
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view: &view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color { r: 0.02, g: 0.025, b: 0.04, a: 1.0 }),
                store: wgpu::StoreOp::Store,
            },
        })],
        depth_stencil_attachment: None,
        timestamp_writes: None,
        occlusion_query_set: None,
        multiview_mask: None,
    });
    let at =
        ItemPlace { rect: [60.0, 40.0, 200.0, 120.0], surface: [W, H], opacity: 0.8, size: Size::new(200.0, 120.0) };
    painter.paint(&mut enc, &view, FORMAT, &Ring, 1.0, &at);
    gpu.queue.submit([enc.finish()]);
    let px = readback::read_rgba(&gpu.device, &gpu.queue, target.texture());

    // Outside the rect the backdrop is untouched.
    let at_px = |x: u32, y: u32| &px[((y * W + x) * 4) as usize..][..4];
    let backdrop = at_px(5, 5).to_vec();
    for (x, y) in [(59, 100), (261, 100), (160, 39), (160, 161)] {
        assert_eq!(at_px(x, y), &backdrop[..], "pixel ({x}, {y}) outside the item changed");
    }
    let img = Image { name: "3d-item".into(), w: W, h: H, channels: 4, px };
    support::check(&img, Tolerance { per_pixel: 3, max_fraction: 0.002 }).unwrap();
}
