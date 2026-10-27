//! 3D on the stage camera (`vo_3d::OnStage`): a 3D ring drawn with the 2D scene's stage
//! camera lines up with a 2D ring laid back by the same angle, off the stage's centre so the
//! lens shift counts, at 1x and 2x. Then the two together with tilted text over them, as a
//! hologram theme would use them.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::f32::consts::FRAC_PI_2;
use std::sync::Arc;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::{Renderer, Target};
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: u32 = 600;
const H: u32 = 300;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// The lay-back of both rings, in radians.
const TILT: f32 = 1.1;
const D: f32 = 200.0;

/// A ring of diameter `D` logical pixels in the item's plane, laid back by `TILT` the way
/// `Element::rotate_x(TILT)` lays a 2D layer back.
#[derive(Debug)]
struct Ring;

impl Source3d for Ring {
    fn meshes(&self) -> Vec<Mesh> {
        vec![Mesh::torus(0.5 * D, 2.5, 128, 10)]
    }

    fn frame(&self, _t: f64, _size: Size) -> Frame3d {
        Frame3d {
            // Flat in xz; up into the item's plane, then the top away from the viewer.
            meshes: vec![MeshDraw {
                mesh: MeshId(0),
                model: Mat4::rotate_x(FRAC_PI_2 - TILT),
                material: Material { color: Color::hex(0x6fe8ff, 1.0), emissive: 1.0, ..Default::default() },
            }],
            on_stage: Some(OnStage { overscan: 0.3 }),
            ..Default::default()
        }
    }

    fn until(&self) -> f64 {
        0.0
    }
}

/// The 2D ring, the 3D ring, or both with a tilted line of text, in a box `D` square whose
/// centre is 140 logical pixels left of the stage origin.
fn view(ring: &Arc<dyn GpuSource>, flat: bool, solid: bool, words: bool) -> Element<()> {
    let mut place = Element::column("place")
        .size(D, D)
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .margin_edges(0.0, 280.0, 0.0, 0.0);
    if flat {
        place = place.child(
            Element::layer("flat")
                .rotate_x(TILT)
                .child(Element::shape("circle", ShapeKind::Circle).size(D, D).stroke(Color::hex(0xff3040, 1.0), 5.0)),
        );
    }
    if solid {
        let top = if flat { -D } else { 0.0 };
        place = place.child(Element::gpu("ring", ring.clone()).size(D, D).margin_edges(top, 0.0, 0.0, 0.0));
    }
    if words {
        // Over the ring's far half, tilted back less than the ring.
        place = place.child(
            Element::layer("words").rotate_x(0.6).margin_edges(-0.75 * D, 0.0, 0.0, 0.0).child(
                Element::label("line", "so the plan")
                    .text_style(TextStyle::new(0, 32.0, 38.0))
                    .color(Color::hex(0xdffaff, 1.0)),
            ),
        );
    }
    Element::panel("root")
        .size(W as f32, H as f32)
        .fill(Color::hex(0x0b0e16, 1.0))
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .justify(vo_scene::taffy::JustifyContent::CENTER)
        .child(place)
}

fn sample(fonts: &FontRegistry, el: Element<()>) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(el, 0.0);
    scene.sample(1.0)
}

/// Bounds `x0, y0, x1, y1` of the pixels `keep` picks.
fn extent(img: &Image, keep: impl Fn([u8; 4]) -> bool) -> [u32; 4] {
    let mut b = [u32::MAX, u32::MAX, 0, 0];
    for y in 0..img.h {
        for x in 0..img.w {
            let i = ((y * img.w + x) * 4) as usize;
            if keep([img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]) {
                b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
            }
        }
    }
    b
}

#[test]
fn a_3d_ring_lines_up_with_a_2d_ring() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let ring = item(Ring);
    let mut r = Renderer::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    install(&mut r);
    let mut render = |list: &DisplayList, name: &str, scale: f32| {
        let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
        let mut target = Target::new(&gpu.device, w, h, FORMAT);
        r.render_at(list, scale, &mut target);
        Image {
            name: name.into(),
            w,
            h,
            channels: 4,
            px: vo_3d::readback::read_rgba(&gpu.device, &gpu.queue, target.texture()),
        }
    };
    let red = |p: [u8; 4]| p[0] > 120 && p[1] < 90;
    let cyan = |p: [u8; 4]| p[2] > 120 && p[0] < 100;
    let mut errors = Vec::new();
    for scale in [1.0, 2.0] {
        let flat = render(&sample(&fonts, view(&ring, true, false, false)), "stage3d-flat", scale);
        let solid = render(&sample(&fonts, view(&ring, false, true, false)), "stage3d-solid", scale);
        let (a, b) = (extent(&flat, red), extent(&solid, cyan));
        // Laid back: much wider than tall, and the far (top) half narrower than the near.
        assert!(a[2] - a[0] > 2 * (a[3] - a[1]), "2D ring {a:?}");
        // Same place within the tube's thickness and a pixel of rounding, scaled.
        let tol = (5.0 * scale) as i64;
        for k in 0..4 {
            let d = a[k] as i64 - b[k] as i64;
            assert!(d.abs() <= tol, "x{scale}: 2D ring {a:?}, 3D ring {b:?}");
        }
        if scale == 1.0 {
            let both = render(&sample(&fonts, view(&ring, true, true, true)), "stage3d-hologram", 1.0);
            let both2 = render(&sample(&fonts, view(&ring, true, true, true)), "stage3d-hologram-2x", 2.0);
            for img in [&both, &both2] {
                if let Err(e) = support::check(img, Tolerance::default()) {
                    errors.push(e);
                }
            }
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
