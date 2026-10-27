//! 3D as a GPU element of a vo scene: `Element::gpu(key, vo_3d::item(..))` drawn by the 2D
//! renderer through `vo_3d::install`, at 1x and 2x, with 2D content after it drawn on top.
//! Then the same through the vo facade, offline, with `Options::renderer_setup`.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::Scene;

const W: u32 = 320;
const H: u32 = 200;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[derive(Debug)]
struct Ring;

impl Source3d for Ring {
    fn meshes(&self) -> Vec<Mesh> {
        vec![Mesh::torus(1.2, 0.05, 96, 12)]
    }

    fn frame(&self, t: f64, _size: Size) -> Frame3d {
        let tilt = Mat4::rotate_x(0.62).then(&Mat4::rotate_y(0.3 * t as f32));
        Frame3d {
            camera: Camera::orbit(Vec3::ZERO, 0.0, 0.1, 4.0),
            meshes: vec![MeshDraw {
                mesh: MeshId(0),
                model: tilt,
                material: Material { color: Color::hex(0x9fc8ff, 1.0), emissive: 0.5, ..Default::default() },
            }],
            ..Default::default()
        }
    }

    fn until(&self) -> f64 {
        2.0
    }
}

/// A backdrop, the 3D ring in a 200 by 120 box at the centre, and a pill drawn after it over
/// the ring's bottom edge.
fn view<M: 'static>(ring: &Arc<dyn GpuSource>) -> Element<M> {
    let column = Element::column("col")
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .child(Element::gpu("ring", ring.clone()).size(200.0, 120.0))
        .child(
            Element::shape("pill", ShapeKind::Capsule)
                .size(90.0, 26.0)
                .offset(0.0, -40.0)
                .fill(Color::hex(0xffd27a, 1.0)),
        );
    Element::panel("root")
        .size(W as f32, H as f32)
        .fill(Color::hex(0x0b0e16, 1.0))
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .justify(vo_scene::taffy::JustifyContent::CENTER)
        .child(column)
}

fn sample(ring: &Arc<dyn GpuSource>, t: f64) -> (DisplayList, Option<f64>) {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(view(ring), 0.0);
    (scene.sample(t), scene.next_wake(t))
}

fn at(px: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [px[i], px[i + 1], px[i + 2], px[i + 3]]
}

#[test]
fn element_draws_in_the_scene() {
    let gpu = support::gpu();
    let ring = item(Ring);
    let (list, wake) = sample(&ring, 1.0);
    assert_eq!(wake, Some(1.0), "the scene wakes until the source's until");
    assert_eq!(sample(&ring, 2.5).1, None, "and settles after it");
    let mut r = Renderer::new(&gpu.device, &gpu.queue);
    install(&mut r);
    let mut errors = Vec::new();
    for (name, scale) in [("3d-element", 1.0f32), ("3d-element-2x", 2.0)] {
        let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
        let mut target = Target::new(&gpu.device, w, h, FORMAT);
        r.render_at(&list, scale, &mut target);
        let px = vo_3d::readback::read_rgba(&gpu.device, &gpu.queue, target.texture());
        // The ring's near edge is bright below the centre of its box; the box is centred
        // above the pill, at y 40..160 before the pill's row.
        let s = |v: u32| (v as f32 * scale) as u32;
        let lit = (s(100)..s(220)).flat_map(|x| (s(20)..s(150)).map(move |y| (x, y))).any(|(x, y)| {
            let p = at(&px, w, x, y);
            p[2] > 150 && p[0] < 200
        });
        assert!(lit, "{name}: no ring pixels");
        // The pill comes after the element, so it is on top: yellow at its centre.
        let pill = at(&px, w, s(160), s(162) - s(40));
        assert!(pill[0] > 200 && pill[2] < 160, "{name}: pill not on top {pill:?}");
        // Far from the box the backdrop is untouched.
        assert_eq!(at(&px, w, 2, 2)[..3], at(&px, w, w - 3, h - 3)[..3], "{name}: backdrop");
        let img = Image { name: name.into(), w, h, channels: 4, px };
        if let Err(e) = support::check(&img, Tolerance::default()) {
            errors.push(e);
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

struct RingApp(Arc<dyn GpuSource>);

impl vo::App for RingApp {
    type Msg = ();

    fn update(&mut self, _: (), _: &mut vo::Cx<()>) {}

    fn view(&self, _: &vo::Tokens) -> Element<()> {
        view(&self.0)
    }
}

/// Through the facade: an app with a 3D element, rendered offline with the painter installed
/// by `Options::renderer_setup`. The same frame as the direct render above.
#[test]
fn offline_app_installs_the_painter() {
    let out = std::path::Path::new(support::OUT_DIR).join("3d-element-offline");
    let _ = std::fs::remove_dir_all(&out);
    let scenario = vo::Scenario { size: Size::new(W as f32, H as f32), scale: 1.0, duration: 1.0, inputs: Vec::new() };
    let options = vo::Options { renderer_setup: Some(Arc::new(install)), ..Default::default() };
    let frames = vo::render_offline_with(RingApp(item(Ring)), &scenario, 4.0, &out, options).expect("render");
    assert!(frames >= 4, "{frames} frames");
    let golden = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/goldens/3d-element.png");
    let decode = |p: &std::path::Path| {
        let mut r =
            png::Decoder::new(std::io::BufReader::new(std::fs::File::open(p).expect("png"))).read_info().expect("png");
        let mut buf = vec![0; r.output_buffer_size().unwrap()];
        r.next_frame(&mut buf).expect("png");
        buf
    };
    // The frame at t = 1 matches the direct render at t = 1.
    let frame = decode(&out.join("frame_00004.png"));
    let want = decode(&golden);
    let off = frame.iter().zip(&want).filter(|(a, b)| a.abs_diff(**b) > 3).count();
    assert!(off < frame.len() / 500, "{off} channels differ from the direct render");
}
