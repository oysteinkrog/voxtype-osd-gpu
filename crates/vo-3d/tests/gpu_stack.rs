//! The text distance field (TY19) and the depth-ordered stack (D304): the field of a word,
//! a point ring that moves away from the word and glows near it, and Slug text on a tilted
//! plane in the scene. GPU test: `just test-gpu` runs it.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use std::f32::consts::FRAC_PI_2;

use support::{Image, Tolerance};
use vo_3d::*;
use vo_render::Target;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: u32 = 320;
const H: u32 = 200;
const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
const TOL: Tolerance = Tolerance { per_pixel: 3, max_fraction: 0.002 };

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

/// A settled display list of one centred word.
fn word(fonts: &FontRegistry, text: &str, size: f32, w: f32, h: f32) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(w, h), 1.0);
    let view = Element::panel("root")
        .size(w, h)
        .align_items(vo_scene::taffy::AlignItems::CENTER)
        .justify(vo_scene::taffy::JustifyContent::CENTER);
    let view =
        view.child(Element::label("w", text).text_style(TextStyle::new(0, size, size * 1.2)).color(Color::WHITE));
    scene.apply(view, 0.0);
    scene.sample(10.0)
}

fn ring(reach: f32) -> PointCloud {
    PointCloud {
        shape: CloudShape::Torus { major: 1.25, minor: 0.18 },
        model: Mat4::rotate_x(FRAC_PI_2 - 0.2),
        count: 9000,
        seed: 3,
        color: Color::hex(0x6fb8ff, 0.85),
        color2: Color::hex(0x9b7bff, 0.85),
        size: 0.02,
        min_px: 1.2,
        t: 1.0,
        speed: 0.3,
        reach_px: reach,
        push_px: 18.0,
        glow: 2.5,
        ..Default::default()
    }
}

fn scene(reach: f32) -> Frame3d {
    Frame3d {
        camera: Camera::orbit(Vec3::ZERO, 0.0, 0.0, 3.6),
        background: Background::Vertical { top: Color::hex(0x0e1016, 1.0), bottom: Color::hex(0x1b1f2a, 1.0) },
        clouds: vec![ring(reach)],
        ..Default::default()
    }
}

fn f16_to_f32(h: u16) -> f32 {
    let (s, e, m) = ((h >> 15) as u32, ((h >> 10) & 0x1f) as u32, (h & 0x3ff) as u32);
    let v = match e {
        0 => m as f32 * 2f32.powi(-24),
        31 => f32::INFINITY,
        _ => (1.0 + m as f32 / 1024.0) * 2f32.powi(e as i32 - 15),
    };
    if s == 1 {
        -v
    } else {
        v
    }
}

#[test]
fn text_field_measures_distance_to_the_words() {
    let gpu = support::gpu();
    let fonts = fonts();
    let text = word(&fonts, "WORDS", 64.0, W as f32, H as f32);
    let mut stack = Stack::new(&gpu.device, &gpu.queue, fonts);
    let target = Target::new(&gpu.device, W, H, FORMAT);
    let layers = Layers { scene: Some(&scene(0.0)), text: Some(&text), field_from_text: true, ..Default::default() };
    stack.render(&layers, &target, 1.0, None);

    let tex = stack.field.texture().expect("field");
    let (fw, fh) = (tex.width(), tex.height());
    assert_eq!((fw, fh), (W / 2, H / 2));
    let raw = readback::read_rgba(&gpu.device, &gpu.queue, tex);
    let d: Vec<f32> = raw.as_chunks::<8>().0.iter().map(|p| f16_to_f32(u16::from_le_bytes([p[0], p[1]]))).collect();
    let at = |x: u32, y: u32| d[(y * fw + x) as usize];
    // Far from the word: about the distance in target pixels to the text box. The word sits
    // in the middle, so a corner is far and the centre row is near or inside.
    assert!(at(0, 0) > 40.0, "corner {}", at(0, 0));
    assert!(d.iter().any(|v| *v < -1.0), "no texel inside the ink");
    // Distances grow by about 2 target pixels per field texel along the top rows above
    // the word (the field is half resolution).
    let (a, b) = (at(fw / 2, 4), at(fw / 2, 0));
    assert!((b - a - 8.0).abs() < 3.0, "gradient {a} -> {b}");

    // Grayscale picture: mid gray on the edge, darker inside, lighter outside.
    let px = d.iter().map(|v| ((0.5 + v / 64.0).clamp(0.0, 1.0) * 255.0) as u8).collect();
    let img = Image { name: "3d-field".into(), w: fw, h: fh, channels: 1, px };
    support::check(&img, Tolerance { per_pixel: 2, max_fraction: 0.002 }).unwrap();
}

#[test]
fn ring_moves_away_from_the_words() {
    let gpu = support::gpu();
    let fonts = fonts();
    let text = word(&fonts, "WORDS", 64.0, W as f32, H as f32);
    let mut stack = Stack::new(&gpu.device, &gpu.queue, fonts);
    let target = Target::new(&gpu.device, W, H, FORMAT);
    let mut images = Vec::new();
    for (name, reach) in [("3d-stack-plain", 0.0), ("3d-stack-react", 36.0)] {
        // The text is drawn too, so this is the D304 order end to end.
        let frame = scene(reach);
        let layers = Layers { scene: Some(&frame), text: Some(&text), field_from_text: true, ..Default::default() };
        stack.render(&layers, &target, 1.0, None);
        images.push(Image {
            name: name.into(),
            w: W,
            h: H,
            channels: 4,
            px: readback::read_rgba(&gpu.device, &gpu.queue, target.texture()),
        });
    }
    let changed =
        images[0].px.as_chunks::<4>().0.iter().zip(images[1].px.as_chunks::<4>().0).filter(|(a, b)| a != b).count();
    assert!(changed > 500, "the field did not move the ring ({changed} pixels changed)");
    let errors: Vec<String> = images.iter().filter_map(|i| support::check(i, TOL).err()).collect();
    assert!(errors.is_empty(), "{}", errors.join("\n"));
}

#[test]
fn slug_text_on_a_tilted_plane() {
    let gpu = support::gpu();
    let fonts = fonts();
    let label = word(&fonts, "Tilted Slug text", 40.0, 400.0, 80.0);
    let mut stack = Stack::new(&gpu.device, &gpu.queue, fonts);
    let target = Target::new(&gpu.device, W, H, FORMAT);
    let frame = Frame3d { camera: Camera::orbit(Vec3::ZERO, 0.5, 0.35, 3.0), ..scene(0.0) };
    // A 400 x 80 list centred on the origin, leaning back 50 degrees.
    let plane = TextPlane {
        list: &label,
        model: Mat4::translation(Vec3::new(-1.0, 0.2, 0.0)).then(&Mat4::rotate_x(-0.9)),
        units_per_px: 0.005,
        opacity: 1.0,
    };
    let planes = [plane];
    let layers = Layers { scene: Some(&frame), planes: &planes, ..Default::default() };
    stack.render(&layers, &target, 1.0, None);
    let img = Image {
        name: "3d-plane".into(),
        w: W,
        h: H,
        channels: 4,
        px: readback::read_rgba(&gpu.device, &gpu.queue, target.texture()),
    };
    support::check(&img, TOL).unwrap();
}
