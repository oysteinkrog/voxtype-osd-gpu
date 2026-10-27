//! The raymarched hero (`vo_3d::hero`): a liquid chrome blob (concept 05), an aurora orb (01)
//! and a jellyfish bell (15) side by side, drawn by the painter `install` adds, at 1x and 2x.
//! Each stays inside its bounding quad; the orb's halo lights the background around it.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::{Renderer, Target};
use vo_scene::display::DisplayList;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::gpu::GpuSource;
use vo_scene::Scene;

const W: u32 = 600;
const H: u32 = 300;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const BACK: u32 = 0x0b0e16;

#[derive(Debug)]
struct Look(HeroFrame);

impl HeroSource for Look {
    fn frame(&self, _t: f64, _size: Size) -> HeroFrame {
        self.0
    }

    fn until(&self) -> f64 {
        0.0
    }
}

fn chrome() -> HeroFrame {
    HeroFrame {
        shape: HeroShape::Blob { lobes: 4 },
        stretch: [1.0, 0.55, 0.8],
        wobble: 0.6,
        phase: 1.3,
        ..Default::default()
    }
}

fn aurora() -> HeroFrame {
    HeroFrame {
        shape: HeroShape::Orb,
        look: HeroLook::Glow {
            a: Color::hex(0x2b6cff, 1.0),
            b: Color::hex(0x8a4dff, 1.0),
            c: Color::hex(0x2de0c8, 1.0),
            rim: Color::hex(0xd8c8ff, 1.0),
            body: 0.85,
            halo: 0.5,
            halo_width: 0.12,
        },
        stretch: [0.8; 3],
        wobble: 0.4,
        phase: 0.7,
        ..Default::default()
    }
}

fn bell() -> HeroFrame {
    HeroFrame {
        shape: HeroShape::Bell,
        look: HeroLook::Glow {
            a: Color::hex(0x1846ff, 1.0),
            b: Color::hex(0x6a3cff, 1.0),
            c: Color::hex(0x23c8ff, 1.0),
            rim: Color::hex(0x9fe8ff, 1.0),
            body: 0.35,
            halo: 0.35,
            halo_width: 0.1,
        },
        stretch: [0.85; 3],
        phase: 0.4,
        ..Default::default()
    }
}

fn view(heroes: &[Arc<dyn GpuSource>]) -> Element<()> {
    let mut row = Element::row("row").gap(30.0).align_items(vo_scene::taffy::AlignItems::CENTER);
    for (i, h) in heroes.iter().enumerate() {
        row = row.child(Element::gpu(format!("hero{i}"), h.clone()).size(160.0, 160.0));
    }
    Element::panel("root")
        .size(W as f32, H as f32)
        .fill(Color::hex(BACK, 1.0))
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .justify(vo_scene::taffy::JustifyContent::CENTER)
        .child(row)
}

fn sample(heroes: &[Arc<dyn GpuSource>]) -> DisplayList {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(view(heroes), 0.0);
    scene.sample(1.0)
}

fn px(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]
}

/// Pixels in `x0..x1` that differ from the background.
fn drawn(img: &Image, x0: u32, x1: u32) -> usize {
    let back = px(img, 2, 2);
    (x0..x1).flat_map(|x| (0..img.h).map(move |y| (x, y))).filter(|&(x, y)| px(img, x, y) != back).count()
}

#[test]
fn three_heroes() {
    let gpu = support::gpu();
    let heroes = [hero(Look(chrome())), hero(Look(aurora())), hero(Look(bell()))];
    let list = sample(&heroes);
    let mut r = Renderer::new(&gpu.device, &gpu.queue);
    install(&mut r);
    let mut errors = Vec::new();
    for (name, scale) in [("hero-three", 1.0f32), ("hero-three-2x", 2.0)] {
        let (w, h) = ((W as f32 * scale) as u32, (H as f32 * scale) as u32);
        let mut target = Target::new(&gpu.device, w, h, FORMAT);
        r.render_at(&list, scale, &mut target);
        let img = Image {
            name: name.into(),
            w,
            h,
            channels: 4,
            px: vo_3d::readback::read_rgba(&gpu.device, &gpu.queue, target.texture()),
        };
        let s = |v: f32| (v * scale) as u32;
        // Boxes at x 45..205, 235..395 and 425..585, centred on y 150.
        for (cx, what) in [(125.0, "blob"), (315.0, "orb"), (505.0, "bell")] {
            let c = px(&img, s(cx), s(150.0));
            assert!(c[..3] != px(&img, 2, 2)[..3], "{name}: the {what} draws nothing at its centre: {c:?}");
        }
        // The chrome blob is bright where it reflects the studio lights and dark elsewhere.
        let lum = |p: [u8; 4]| p[0] as u32 + p[1] as u32 + p[2] as u32;
        let blob: Vec<u32> = (s(45.0)..s(205.0))
            .flat_map(|x| (s(100.0)..s(200.0)).map(move |y| (x, y)))
            .map(|(x, y)| lum(px(&img, x, y)))
            .collect();
        assert!(blob.iter().any(|&l| l > 600) && blob.iter().any(|&l| l < 150), "{name}: chrome lacks contrast");
        // The orb's halo: just outside its edge (radius 0.8 * 80) the background is lit.
        let out = px(&img, s(315.0 + 72.0), s(150.0));
        assert!(lum(out) > lum(px(&img, 2, 2)) + 6, "{name}: no halo {out:?}");
        // Nothing far from the three.
        assert_eq!(drawn(&img, 0, s(20.0)), 0, "{name}: ink at the left edge");
        if let Err(e) = support::check(&img, Tolerance::default()) {
            errors.push(e);
        }
    }
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}
