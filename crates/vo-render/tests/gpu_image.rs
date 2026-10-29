//! Images (EG-24): a PNG and a full-colour SVG drawn by the built-in image painter.
//!
//! The test images are made in code: a 16 x 16 PNG of four quarters (red, green, blue at half
//! alpha, white fading out) and an SVG with a gradient panel and a half-transparent disc. They
//! sit on a dark panel; a second PNG copy sits in a turned layer at half opacity, and the SVG
//! again as a 14-unit icon in a pill, as a receipt chip would show it. Goldens at scale 1 and
//! 1.5, and in light mode on a light panel.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, ShapeKind};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::image::ImageSource;
use vo_scene::taffy;
use vo_scene::Scene;

type El = Element<()>;

const W: f32 = 240.0;
const H: f32 = 96.0;

/// Four quarters: red, green; blue at half alpha, white fading out left to right.
fn quarters_png() -> Vec<u8> {
    let mut px = Vec::new();
    for y in 0..16u32 {
        for x in 0..16u32 {
            let p: [u8; 4] = match (x < 8, y < 8) {
                (true, true) => [255, 0, 0, 255],
                (false, true) => [0, 255, 0, 255],
                (true, false) => [0, 0, 255, 128],
                (false, false) => [255, 255, 255, (255 - (x - 8) * 30) as u8],
            };
            px.extend_from_slice(&p);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 16, 16);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&px).unwrap();
    out
}

const SVG: &str = r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 24" width="32" height="24">
<defs><linearGradient id="g" x1="0" y1="0" x2="1" y2="0">
<stop offset="0" stop-color="#ff8800"/><stop offset="1" stop-color="#8800ff"/></linearGradient></defs>
<rect x="1" y="1" width="30" height="22" rx="5" fill="url(#g)"/>
<circle cx="16" cy="12" r="7" fill="#ffffff" fill-opacity="0.5"/></svg>"##;

struct Sources {
    png: Arc<ImageSource>,
    svg: Arc<ImageSource>,
}

fn sources() -> Sources {
    Sources { png: ImageSource::bytes(quarters_png()), svg: ImageSource::bytes(SVG.as_bytes().to_vec()) }
}

/// `e` placed at `x, y` in its parent, out of the flow.
fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

fn list(src: &Sources, scale: f32, light: bool) -> DisplayList {
    let bg = if light { Color::hex(0xf4f1ea, 1.0) } else { Color::hex(0x07080c, 1.0) };
    let turned = Element::layer("turned")
        .size(40.0, 40.0)
        .rotate(0.35)
        .opacity(0.5)
        .child(Element::image("png2", src.png.clone()).size(40.0, 40.0));
    let chip = Element::row("chip")
        .size(60.0, 22.0)
        .align_items(taffy::AlignItems::CENTER)
        .justify(taffy::JustifyContent::CENTER)
        .child(Element::shape("pill", ShapeKind::Capsule).size(60.0, 22.0).fill(Color::hex(0x334455, 1.0)).layout(
            |s| {
                s.position = taffy::style::Position::Absolute;
            },
        ))
        .child(Element::image("icon", src.svg.clone()).size(14.0, 14.0));
    let root = Element::panel("root")
        .size(W, H)
        .fill(bg)
        .child(at(Element::image("png", src.png.clone()).size(48.0, 48.0), 12.0, 12.0))
        // A wide box: the SVG keeps its 4:3 aspect, centred.
        .child(at(Element::image("svg", src.svg.clone()).size(80.0, 48.0), 70.0, 12.0))
        .child(at(turned, 168.0, 10.0))
        .child(at(chip, 166.0, 64.0));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), scale);
    scene.apply(root, 0.0);
    assert_eq!(scene.next_wake(1.0), None, "an image never keeps the scene awake");
    scene.sample(0.5)
}

fn render(off: &mut Offscreen, src: &Sources, name: &str, scale: f32, light: bool) -> Image {
    off.renderer.set_light(light);
    let list = list(src, scale, light);
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(&list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

fn rgba(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]
}

#[test]
fn image_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let src = sources();
    let images = [
        render(&mut off, &src, "image-1x", 1.0, false),
        render(&mut off, &src, "image-1.5x", 1.5, false),
        render(&mut off, &src, "image-light-1x", 1.0, true),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// Colours come through sRGB-correct and premultiplied: a red PNG pixel reads 255, 0, 0; blue
/// at half alpha over the dark panel reads about half blue; the SVG gradient runs orange to
/// violet across its box.
#[test]
fn images_draw_their_colours() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let src = sources();
    let img = render(&mut off, &src, "probe", 1.0, false);
    // The PNG is x 12 to 60, y 12 to 60; its quarters are 24 units.
    assert_eq!(rgba(&img, 20, 20), [255, 0, 0, 255], "red");
    assert_eq!(rgba(&img, 50, 20), [0, 255, 0, 255], "green");
    let blue = rgba(&img, 20, 50);
    assert!(blue[0] < 12 && blue[1] < 12 && (150..205).contains(&blue[2]), "half blue over dark {blue:?}");
    // The SVG: 64 x 48 centred in the 80 x 48 box at x 70, so x 78 to 142.
    let left = rgba(&img, 84, 36);
    let right = rgba(&img, 136, 36);
    assert!(left[0] > 220 && left[2] < 60, "orange end {left:?}");
    assert!(right[2] > 220 && right[1] < 60, "violet end {right:?}");
    // Outside the fitted SVG the panel shows.
    assert!(rgba(&img, 73, 36)[0] < 20, "the box past the fitted image is clear");
    // The chip's icon: the gradient shows inside the pill.
    let icon = rgba(&img, 191, 75);
    assert!(icon[0] > 120, "icon drawn in the chip {icon:?}");
}

/// A `w` by `h` PNG of a diagonal gradient, its content salted by `salt` so no other test
/// shares its cache entries.
fn gradient_png(w: u32, h: u32, salt: u8) -> Vec<u8> {
    let mut px = Vec::with_capacity((4 * w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, salt, 255]);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    enc.write_header().unwrap().write_image_data(&px).unwrap();
    out
}

/// One frame: `src` in a `box_size` box inside a layer scaled by `scale`, on a fresh scene.
fn scaled_frame(off: &mut Offscreen, src: &Arc<ImageSource>, box_size: f32, scale: f32) -> Image {
    let layer =
        Element::layer("l").size(box_size, box_size).scale(scale).child(Element::image("img", src.clone()).size(box_size, box_size));
    let root = Element::panel("root").size(box_size, box_size).child(layer);
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(box_size, box_size), 1.0);
    scene.apply(root, 0.0);
    let list = scene.sample(0.0);
    let n = box_size as u32;
    let px = off.render_to_rgba(&list, [n, n], 1.0);
    Image { name: "probe".into(), w: n, h: n, channels: 4, px }
}

/// RV-03 (a): a raster too big for the 8 MB decode cache (1500 x 1500 x 4 bytes) is decoded
/// once and then drawn from the painter's texture, not decoded every frame.
#[test]
fn a_big_image_painted_ten_frames_decodes_once() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let src = ImageSource::bytes(gradient_png(2000, 2000, 7));
    for _ in 0..10 {
        scaled_frame(&mut off, &src, 1500.0, 1.0);
    }
    assert_eq!(src.decodes(), 1);
}

/// RV-03 (b): a layer scale animating 0.8 to 1.0 over 20 frames (a pop-in) decodes at most
/// twice, and the image still fills its scaled box.
#[test]
fn a_pop_in_scale_decodes_at_most_twice() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let src = ImageSource::bytes(gradient_png(256, 256, 9));
    let mut last = None;
    for f in 0..=20 {
        let img = scaled_frame(&mut off, &src, 100.0, 0.8 + 0.2 * f as f32 / 20.0);
        if f == 0 {
            assert_eq!(rgba(&img, 2, 2)[3], 0, "at 0.8 the scaled box leaves the corner clear");
        }
        last = Some(img);
    }
    assert!(src.decodes() <= 2, "{} decodes", src.decodes());
    let img = last.unwrap();
    assert_eq!(rgba(&img, 2, 2)[3], 255, "the image covers the box at rest");
    assert!(rgba(&img, 97, 50)[0] > 230, "the right edge is the gradient's red end");
}

/// RV-03 (9): a file rewritten at the same path draws its new pixels.
#[test]
fn a_rewritten_file_draws_its_new_pixels() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("gpu_image");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rewrite.png");
    let solid = |p: [u8; 4]| {
        let px: Vec<u8> = (0..16).flat_map(|_| p).collect();
        let mut out = Vec::new();
        let mut enc = png::Encoder::new(&mut out, 4, 4);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.write_header().unwrap().write_image_data(&px).unwrap();
        out
    };
    let write = |bytes: Vec<u8>, secs: u64| {
        std::fs::write(&path, bytes).unwrap();
        let f = std::fs::File::options().write(true).open(&path).unwrap();
        f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs)).unwrap();
    };
    write(solid([255, 0, 0, 255]), 2_000_000);
    let src = ImageSource::file(&path);
    assert_eq!(rgba(&scaled_frame(&mut off, &src, 16.0, 1.0), 8, 8), [255, 0, 0, 255]);
    write(solid([0, 0, 255, 255]), 2_000_001);
    assert_eq!(rgba(&scaled_frame(&mut off, &src, 16.0, 1.0), 8, 8), [0, 0, 255, 255]);
    std::fs::remove_file(&path).unwrap();
}
