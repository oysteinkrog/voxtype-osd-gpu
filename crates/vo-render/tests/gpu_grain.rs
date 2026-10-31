//! Paper, fabric, rubber and film grain (EG-25).
//!
//! Four 200 x 100 panels, one per grain material, on a plain backing: paper top left, fabric
//! top right, rubber bottom left, film grain bottom right. Goldens at scale 1 and 1.5, and in
//! light mode on light panels. Film grain holds still within a 24th of a second, moves at
//! the next, and stands still under reduced motion. The CPU first frame draws the plain
//! panels.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::material::Material;
use vo_scene::Scene;

type El = Element<()>;

const W: f32 = 400.0;
const H: f32 = 200.0;

fn panels(light: bool) -> El {
    let fill = if light { Color::hex(0xefe8d8, 1.0) } else { Color::hex(0x3a3d48, 1.0) };
    let panel = |key: &'static str, m: Material| Element::panel(key).size(200.0, 100.0).fill(fill).material(m);
    let row = |key: &'static str, a: El, b: El| Element::row(key).child(a).child(b);
    Element::panel("root")
        .size(W, H)
        .fill(Color::hex(0x101014, 1.0))
        .child(row("top", panel("paper", Material::paper().into()), panel("fabric", Material::fabric().into())))
        .child(row(
            "bottom",
            panel("rubber", Material::rubber().into()),
            panel("grain", Material::film_grain().into()),
        ))
}

fn list(scale: f32, light: bool, t: f64, reduced: bool) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W, H), scale);
    scene.apply(panels(light), 0.0);
    scene.sample(t)
}

fn render(off: &mut Offscreen, name: &str, scale: f32, light: bool, t: f64, reduced: bool) -> Image {
    off.renderer.set_light(light);
    let list = list(scale, light, t, reduced);
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(&list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

/// The RGB bytes of the box `x, y, w, h` (target pixels).
fn crop(img: &Image, x: u32, y: u32, w: u32, h: u32) -> Vec<u8> {
    let mut out = Vec::new();
    for yy in y..y + h {
        for xx in x..x + w {
            let i = ((yy * img.w + xx) * 4) as usize;
            out.extend_from_slice(&img.px[i..i + 3]);
        }
    }
    out
}

fn spread(px: &[u8]) -> (f64, f64) {
    let n = px.len() as f64;
    let mean = px.iter().map(|&v| v as f64).sum::<f64>() / n;
    let sd = (px.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / n).sqrt();
    (mean, sd)
}

#[test]
fn grain_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let images = [
        render(&mut off, "grain-1x", 1.0, false, 1.0, false),
        render(&mut off, "grain-1.5x", 1.5, false, 1.0, false),
        render(&mut off, "grain-light-1x", 1.0, true, 1.0, false),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// Each tile moves the paint around its colour: the panel is no longer flat, and its mean
/// stays near the plain fill. Fabric shows the most, film grain the least.
#[test]
fn every_tile_textures_its_panel() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let img = render(&mut off, "probe", 1.0, false, 1.0, false);
    let plain = [0x3a, 0x3d, 0x48];
    let mut sds = Vec::new();
    for (name, x, y) in [("paper", 0, 0), ("fabric", 200, 0), ("rubber", 0, 100), ("grain", 200, 100)] {
        let px = crop(&img, x + 8, y + 8, 184, 84);
        let (mean, sd) = spread(&px);
        let flat = plain.iter().map(|&v| v as f64).sum::<f64>() / 3.0;
        assert!(sd > 1.5, "{name}: flat, sd {sd:.2}");
        assert!((mean - flat).abs() < 12.0, "{name}: mean {mean:.1} against {flat:.1}");
        sds.push(sd);
    }
    assert!(sds[1] > sds[3], "fabric {:.2} shows more than film grain {:.2}", sds[1], sds[3]);
}

/// The grain clock: the same frame at t and t + 0.01 s, a new one at t + 1/24 s; under reduced
/// motion the grain stands still.
#[test]
fn film_grain_steps_24_times_a_second() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let grain = |off: &mut Offscreen, t: f64, reduced: bool| {
        crop(&render(off, "probe", 1.0, false, t, reduced), 200, 100, 200, 100)
    };
    let t = 2.0;
    let a = grain(&mut off, t, false);
    assert_eq!(a, grain(&mut off, t + 0.01, false), "the same frame within a 24th");
    assert_ne!(a, grain(&mut off, t + 1.0 / 24.0, false), "a new frame at the next 24th");
    let still = grain(&mut off, t, true);
    assert_eq!(still, grain(&mut off, t + 1.0 / 24.0, true), "still under reduced motion");
    // The paper does not move at all.
    let paper = |off: &mut Offscreen, t: f64| crop(&render(off, "probe", 1.0, false, t, false), 0, 0, 200, 100);
    assert_eq!(paper(&mut off, t), paper(&mut off, t + 1.0 / 24.0));
}

/// The CPU first frame draws each panel in its plain fill.
#[cfg(feature = "cpu")]
#[test]
fn the_cpu_first_frame_draws_the_plain_panels() {
    let list = list(1.0, false, 0.0, false);
    let mut cpu = vo_render::cpu::CpuRenderer::new(vo_scene::font::FontRegistry::new());
    let (w, h) = (W as u32, H as u32);
    let mut got = vec![0u8; (w * h * 4) as usize];
    cpu.draw_bgra(&list, 1.0, w, h, w as usize * 4, &mut got);
    for (x, y) in [(100, 50), (300, 50), (100, 150), (300, 150), (20, 20), (380, 180)] {
        let i = ((y * w + x) * 4) as usize;
        let [b, g, r] = [got[i], got[i + 1], got[i + 2]];
        assert!(
            r.abs_diff(0x3a) <= 1 && g.abs_diff(0x3d) <= 1 && b.abs_diff(0x48) <= 1,
            "{x},{y}: {r} {g} {b}"
        );
    }
}
