//! Materials that read textures (EG-02).
//!
//! A striped panel with a card over it. The card is a layer whose backdrop only captures (a
//! transparent tint), and the card's panel has an app material that reads that backdrop
//! tinted blue and shifted 4 pixels right. A second panel has a material that stretches a
//! 2x2 checker from image slot 0 over itself. A third panel reads a backdrop its layer does
//! not have: it draws transparent black, and the frame still renders.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{BackdropRes, DisplayList};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::material::{Material, CUSTOM_FIRST};
use vo_scene::taffy;
use vo_scene::Scene;

type El = Element<()>;

const W: f32 = 320.0;
const H: f32 = 120.0;
const SHIFT: u32 = CUSTOM_FIRST;
const CHECKER: u32 = CUSTOM_FIRST + 1;

/// The backdrop, tinted by colour slot 0 and moved 4 target pixels right.
const SHIFT_WGSL: &str = "
fn mat_tint_shift (m: Material, i: MatIn) -> vec4f {
  let b = tex_backdrop (i.px - vec2f (4.0, 0.0));
  let tint = mat_lab (m.p[0]).rgb;
  return vec4f (b.rgb * tint, b.a) * (i.cov * i.color.a);
}
";

/// Image slot 0 stretched over the box `p[1]` (x, y, w, h in layer units).
const CHECKER_WGSL: &str = "
fn mat_checker (m: Material, i: MatIn) -> vec4f {
  let uv = (i.p - m.p[1].xy) / m.p[1].zw;
  return tex_image (0u, uv) * (i.cov * i.color.a);
}
";

/// `e` placed at `x, y` in its parent, out of the flow.
fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

/// Vertical stripes 8 units wide, white and red.
fn stripes(n: u32, h: f32) -> El {
    Element::row("stripes").children((0..n).map(|i| {
        let c = if i % 2 == 0 { Color::WHITE } else { Color::hex(0xff3030, 1.0) };
        Element::panel(i as u64).size(8.0, h).fill(c)
    }))
}

fn list(scale: f32) -> DisplayList {
    let blue = Material::custom(SHIFT).reads_backdrop().color(0, Color::hex(0x4080ff, 1.0));
    // The card captures what is under it without drawing it back; its panel reads it.
    let card = Element::layer("card")
        .size(96.0, 48.0)
        .backdrop(BackdropRes::Full, Color::TRANSPARENT)
        .child(Element::panel("glass").size(96.0, 48.0).radius(12.0).fill(Color::WHITE).material(blue));
    let checker = Material::custom(CHECKER).reads_images().param(1, [180.0, 30.0, 40.0, 40.0]);
    // Reads a backdrop, but no layer above it has one.
    let lost = Material::custom(SHIFT).reads_backdrop().color(0, Color::WHITE);
    let root = Element::panel("root")
        .size(W, H)
        .fill(Color::hex(0x07080c, 1.0))
        .child(at(stripes(16, 80.0), 20.0, 20.0))
        .child(at(card, 40.0, 36.0))
        .child(at(Element::panel("checker").size(40.0, 40.0).fill(Color::WHITE).material(checker), 180.0, 30.0))
        .child(at(Element::panel("lost").size(40.0, 40.0).fill(Color::WHITE).material(lost), 250.0, 30.0));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), scale);
    scene.apply(root, 0.0);
    scene.sample(0.5)
}

/// A 2x2 checker: red, green on top, blue, white below.
fn checker_view(gpu: &support::Gpu) -> wgpu::TextureView {
    let size = wgpu::Extent3d { width: 2, height: 2, depth_or_array_layers: 1 };
    let tex = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("checker"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8Unorm,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let px: [u8; 16] = [255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255];
    gpu.queue.write_texture(
        tex.as_image_copy(),
        &px,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8), rows_per_image: Some(2) },
        size,
    );
    tex.create_view(&Default::default())
}

fn setup(gpu: &support::Gpu) -> Offscreen {
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    off.renderer.add_material(SHIFT, "mat_tint_shift", SHIFT_WGSL);
    off.renderer.add_material(CHECKER, "mat_checker", CHECKER_WGSL);
    off.renderer.set_material_image(0, Some(checker_view(gpu)));
    off
}

fn render(off: &mut Offscreen, name: &str, scale: f32) -> Image {
    let list = list(scale);
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(&list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

fn rgba(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]
}

#[test]
fn material_texture_goldens() {
    let gpu = support::gpu();
    let mut off = setup(&gpu);
    let images = [render(&mut off, "mt-1x", 1.0), render(&mut off, "mt-1.5x", 1.5)];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// Under the card, a white stripe reads blue and a red one dark, 4 pixels right of where the
/// stripes sit; the checker shows its four colours; the lost material draws nothing.
#[test]
fn materials_read_their_textures() {
    let gpu = support::gpu();
    let mut off = setup(&gpu);
    let img = render(&mut off, "probe", 1.0);
    // Stripe i covers x 20 + 8 i to 28 + 8 i: stripe 3 (red) is x 44 to 52, stripe 4 (white)
    // 52 to 60. Shifted 4 right, x 57 to 64 reads white (blue) and x 49 to 56 red (dark).
    let white = rgba(&img, 60, 60);
    assert!(white[2] > 240 && white[0] < 140 && white[1] < 180, "white stripe tinted blue {white:?}");
    let red = rgba(&img, 53, 60);
    assert!(red.iter().take(3).all(|&v| v < 80), "red stripe tinted dark {red:?}");
    // x 54 reads white unshifted; shifted, it reads the red stripe at x 50.
    let edge = rgba(&img, 54, 60);
    assert!(edge[2] < 80, "the backdrop is shifted 4 px {edge:?}");
    // The checker's quarters (the panel is x 180 to 220, y 30 to 70).
    let q = |x, y| rgba(&img, x, y);
    assert!(q(184, 34)[0] > 200 && q(184, 34)[1] < 60, "top left red {:?}", q(184, 34));
    assert!(q(216, 34)[1] > 200 && q(216, 34)[0] < 60, "top right green {:?}", q(216, 34));
    assert!(q(184, 66)[2] > 200 && q(184, 66)[0] < 60, "bottom left blue {:?}", q(184, 66));
    assert!(q(216, 66).iter().all(|&v| v > 200), "bottom right white {:?}", q(216, 66));
    // The lost material: the panel's background shows through (x 250 to 290).
    let lost = rgba(&img, 270, 50);
    assert!(lost[0] < 20 && lost[1] < 20 && lost[2] < 30, "a missing backdrop is transparent {lost:?}");
}
