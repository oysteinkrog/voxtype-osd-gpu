//! The grid floor (`vo_3d::grid_floor`, FX-152): lines running to a vanishing point under a
//! horizon glow, with a pool of light, at 1x and 2x. Scrolling by one whole cell gives the
//! same picture; half a cell does not. Nothing is drawn well above the horizon, and the
//! lines spread out toward the viewer.
//!
//! GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::{Renderer, Target};
use vo_scene::display::DisplayList;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::Scene;

const W: u32 = 600;
const H: u32 = 300;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

#[derive(Debug)]
struct Floor(f32);

impl GridSource for Floor {
    fn frame(&self, _t: f64, _size: Size) -> GridFrame {
        GridFrame { scroll: self.0, ..Default::default() }
    }

    fn until(&self) -> f64 {
        0.0
    }
}

fn sample(scroll: f32) -> DisplayList {
    let view = Element::panel("root")
        .size(W as f32, H as f32)
        .fill(Color::hex(0x07041a, 1.0))
        .child(Element::gpu("floor", grid_floor(Floor(scroll))).size(W as f32, H as f32));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W as f32, H as f32), 1.0);
    scene.apply(view, 0.0);
    scene.sample(1.0)
}

fn lum(img: &Image, x: u32, y: u32) -> u32 {
    let i = ((y * img.w + x) * 4) as usize;
    img.px[i] as u32 + img.px[i + 1] as u32 + img.px[i + 2] as u32
}

/// Bright runs along row `y`: how many lines cross it.
fn crossings(img: &Image, y: u32) -> usize {
    let row: Vec<u32> = (0..img.w).map(|x| lum(img, x, y)).collect();
    let mut sorted = row.clone();
    sorted.sort_unstable();
    let cut = sorted[sorted.len() / 2] + 40;
    row.windows(2).filter(|w| w[0] < cut && w[1] >= cut).count()
}

#[test]
fn grid_floor_in_perspective() {
    let gpu = support::gpu();
    let mut r = Renderer::new(&gpu.device, &gpu.queue);
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
    let cell = GridFrame::default().cell;
    let base = render(&sample(0.0), "grid-floor", 1.0);
    let whole = render(&sample(cell), "grid-floor-cell", 1.0);
    let half = render(&sample(0.5 * cell), "grid-floor-half", 1.0);
    let hi = render(&sample(0.0), "grid-floor-2x", 2.0);
    // The goldens first, so a failed check below still leaves the pictures to look at.
    let failures: Vec<String> =
        [&base, &hi].iter().filter_map(|i| support::check(i, Tolerance::default()).err()).collect();
    let differ = |a: &Image, b: &Image| a.px.iter().zip(&b.px).filter(|(p, q)| p.abs_diff(**q) > 2).count();
    assert!(differ(&base, &whole) < 50, "one cell of scroll should repeat: {} px differ", differ(&base, &whole));
    assert!(differ(&base, &half) > 2000, "half a cell should move the lines: {} px differ", differ(&base, &half));

    // Well above the horizon (at 35% of the height) only the background shows.
    let back = lum(&base, 2, 2);
    assert!((0..W).all(|x| lum(&base, x, 40) <= back + 3), "ink above the horizon");
    // Near the horizon the glow lights the row.
    assert!(lum(&base, W / 2, (0.35 * H as f32) as u32) > back + 30, "no horizon glow");
    // Lines converge: fewer, wider-spaced crossings near the viewer than near the horizon.
    // Just under the horizon the lines have faded into the glow, so the far row is a little
    // lower, where they show.
    let (far, near) = (crossings(&base, 145), crossings(&base, 290));
    assert!(far > near && near >= 4, "crossings near the horizon {far}, at the bottom {near}");

    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
