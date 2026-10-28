//! Effect composition on one layer: shaped bloom, bloom with a style and a mask, nested
//! rounded clips, blend modes on both paths, and the dither of slow gradients.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{BlendMode, BloomShape, DisplayList, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Size};
use vo_scene::taffy;
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Bold.ttf";

type El = Element<()>;

/// `e` placed at `x, y` in its parent, out of the flow.
fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

fn pink() -> Color {
    Color::hex(0xff2bd6, 1.0)
}

fn cyan() -> Color {
    Color::hex(0x22e4ff, 1.0)
}

/// Thin neon lines and a word: a rounded tube, a ring and a rule.
fn neon(key: &'static str) -> El {
    Element::panel(key)
        .size(200.0, 110.0)
        .child(at(Element::panel("tube").size(180.0, 60.0).radius(30.0).stroke(pink(), 2.0), 10.0, 10.0))
        .child(at(Element::shape("ring", ShapeKind::Circle).size(36.0, 36.0).stroke(cyan(), 1.5), 150.0, 64.0))
        .child(at(Element::panel("rule").size(120.0, 1.5).fill(cyan()), 16.0, 94.0))
        .child(at(
            Element::label("word", "NEON").text_style(TextStyle::new(0, 26.0, 30.0)).color(Color::hex(0xffe8fb, 1.0)),
            62.0,
            24.0,
        ))
}

fn backdrop(key: &'static str, w: f32, h: f32) -> El {
    Element::panel(key).size(w, h).fill(Color::hex(0x07080c, 1.0))
}

fn scene_of(view: El, w: f32, h: f32) -> DisplayList {
    let fonts = FontRegistry::new();
    fonts.add_file(FONT.as_ref()).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"));
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts)));
    scene.set_viewport(0.0, Size::new(w, h), 1.0);
    scene.apply(view, 0.0);
    scene.sample(0.5)
}

/// The same neon twice: a plain bloom on the left, the shaped bloom on the right.
pub fn bloom_list() -> DisplayList {
    let plain = Element::layer("plain").size(220.0, 130.0).pad(10.0).bloom(1.0, 16.0).child(neon("n1"));
    let shaped = Element::layer("shaped")
        .size(220.0, 130.0)
        .pad(10.0)
        .bloom(1.0, 16.0)
        .bloom_shape(BloomShape::NEON)
        .child(neon("n2"));
    scene_of(
        backdrop("root", 460.0, 150.0)
            .pad(10.0)
            .gap(20.0)
            .layout(|s| s.flex_direction = taffy::prelude::FlexDirection::Row)
            .children([plain, shaped]),
        460.0,
        150.0,
    )
}

/// One layer with bloom, a style and a mask, inside two nested rounded clips; and a nested
/// clip alone, whose inner layer is larger than the outer one's rounded box.
pub fn stack_list() -> DisplayList {
    let styled = Element::layer("styled")
        .size(220.0, 130.0)
        .pad(10.0)
        .bloom(1.0, 12.0)
        .scanlines(0.5, 3.0)
        .crt(0.12)
        .chromatic(2.0)
        .child(neon("n"))
        .child(Element::mask("m").child(at(
            Element::panel("hole").size(200.0, 110.0).radius(40.0).fill(Color::WHITE),
            10.0,
            10.0,
        )));
    let inner = Element::layer("inner")
        .size(240.0, 150.0)
        .radius(6.0)
        .clip()
        .offset(-10.0, -10.0)
        .child(Element::panel("bright").size(240.0, 150.0).fill(Color::hex(0x3a86ff, 1.0)));
    let nested = Element::layer("outer").size(220.0, 130.0).radius(36.0).clip().child(inner);
    scene_of(
        backdrop("root", 480.0, 150.0)
            .pad(10.0)
            .gap(20.0)
            .layout(|s| s.flex_direction = taffy::prelude::FlexDirection::Row)
            .children([styled, nested]),
        480.0,
        150.0,
    )
}

/// Overlapping discs in an `Add`, a `Screen` and a `Multiply` layer, each half over a striped
/// backdrop and half over nothing. With `effects`, a small bloom elsewhere puts the frame on
/// the effect path.
pub fn blend_list(effects: bool) -> DisplayList {
    let stripes = |key: &'static str| {
        Element::row(key).children(
            [0xe63946u32, 0xf1c453, 0x2a9d8f, 0x457b9d, 0xf4f1de]
                .map(|c| Element::panel(c as u64).size(24.0, 70.0).fill(Color::hex(c, 1.0))),
        )
    };
    let discs = |key: &'static str, mode: BlendMode| {
        Element::layer(key)
            .size(120.0, 120.0)
            .blend(mode)
            .child(at(
                Element::shape("a", ShapeKind::Circle).size(70.0, 70.0).fill(Color::hex(0x6a4c93, 0.9)),
                0.0,
                0.0,
            ))
            .child(at(
                Element::shape("b", ShapeKind::Circle).size(70.0, 70.0).fill(Color::hex(0x8ac926, 0.9)),
                40.0,
                40.0,
            ))
    };
    let cell = |key: &'static str, mode: BlendMode| {
        Element::panel(key).size(150.0, 130.0).child(at(stripes("s"), 0.0, 0.0)).child(at(discs("d", mode), 20.0, 10.0))
    };
    let mut root = Element::panel("root")
        .size(470.0, 150.0)
        .pad(10.0)
        .gap(10.0)
        .layout(|s| s.flex_direction = taffy::prelude::FlexDirection::Row)
        .children([cell("add", BlendMode::Add), cell("screen", BlendMode::Screen), cell("mul", BlendMode::Multiply)]);
    if effects {
        root = root.child(at(
            Element::layer("spark")
                .size(6.0, 6.0)
                .bloom(0.5, 2.0)
                .child(Element::panel("dot").size(6.0, 6.0).fill(Color::WHITE)),
            460.0,
            140.0,
        ));
    }
    scene_of(root, 470.0, 150.0)
}

/// A slow dark gradient, top to bottom, over 300 pixels: about 14 sRGB codes, so each band
/// is about 20 pixels tall without dither.
pub fn dither_list(effects: bool) -> DisplayList {
    let mut root =
        Element::panel("root").size(160.0, 300.0).fill_gradient(Color::hex(0x10141c, 1.0), Color::hex(0x1e2433, 1.0));
    if effects {
        root = root.child(at(
            Element::layer("spark")
                .size(6.0, 6.0)
                .bloom(0.5, 2.0)
                .child(Element::panel("dot").size(6.0, 6.0).fill(Color::WHITE)),
            150.0,
            290.0,
        ));
    }
    scene_of(root, 160.0, 300.0)
}

fn offscreen(gpu: &support::Gpu) -> Offscreen {
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    assert_eq!(off.renderer.add_font(std::fs::read(FONT).unwrap()).unwrap(), 0);
    off
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

fn rgba(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.w + x) * 4) as usize;
    [img.px[i], img.px[i + 1], img.px[i + 2], img.px[i + 3]]
}

#[test]
fn compose_goldens() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let images = [
        render(&mut off, "cmp-bloom", &bloom_list(), 1.0),
        render(&mut off, "cmp-stack", &stack_list(), 1.0),
        render(&mut off, "cmp-stack-1.5x", &stack_list(), 1.5),
        render(&mut off, "cmp-blend-direct", &blend_list(false), 1.0),
        render(&mut off, "cmp-blend-effect", &blend_list(true), 1.0),
        render(&mut off, "cmp-dither-direct", &dither_list(false), 1.0),
        render(&mut off, "cmp-dither-effect", &dither_list(true), 1.0),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// The outer clip's rounded corner cuts the larger inner layer: nothing shows in the corner.
#[test]
fn nested_clip_keeps_the_outer_corner_round() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let img = render(&mut off, "nested", &stack_list(), 1.0);
    // The nested card's box starts at x 250, y 10; its clip radius is 36.
    let corner = rgba(&img, 250 + 3, 10 + 3);
    let backdrop = rgba(&img, 2, 2);
    assert_eq!(corner, backdrop, "the outer corner shows the backdrop");
    let inside = rgba(&img, 250 + 40, 10 + 40);
    assert!(inside[2] > 200, "inside, the inner layer shows: {inside:?}");
}

/// Sum of red, green and blue over the 5 by 5 box centred on `x, y`.
fn light(img: &Image, x: u32, y: u32) -> u32 {
    let mut sum = 0;
    for yy in y - 2..=y + 2 {
        for xx in x - 2..=x + 2 {
            let p = rgba(img, xx, yy);
            sum += p[0] as u32 + p[1] as u32 + p[2] as u32;
        }
    }
    sum
}

/// The shaped bloom's wide tail is fainter than the plain bloom's, 16 pixels off the tube.
#[test]
fn shaped_bloom_has_a_fainter_tail() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let img = render(&mut off, "tail", &bloom_list(), 1.0);
    // The tubes' top edges run along y 30, from x 30 (left) and x 270 (right).
    let (plain, shaped) = (light(&img, 120, 14), light(&img, 360, 14));
    let dark = light(&img, 2 + 2, 2 + 2);
    assert!(plain > dark + 100, "the plain bloom reaches 16 px: {plain} vs {dark}");
    assert!(shaped < plain, "shaped tail {shaped} is fainter than plain {plain}");
}

/// A layer with a mask, a style and bloom still glows: the mask no longer drops the bloom.
#[test]
fn masked_styled_layer_keeps_its_bloom() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let img = render(&mut off, "glow", &stack_list(), 1.0);
    // 4 px above the tube's top edge (y 30), inside the mask's hole.
    let near = light(&img, 120, 26);
    let dark = light(&img, 4, 4);
    assert!(near > dark + 150, "glow beside the tube: {near} vs backdrop {dark}");
}

/// Multiply over nothing shows the layer's own colour, on both paths.
#[test]
fn multiply_over_transparent_keeps_the_colour() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    for effects in [false, true] {
        let img = render(&mut off, "mul", &blend_list(effects), 1.0);
        // Inside the green disc of the multiply cell (the third, from x 316), below the stripes.
        let p = rgba(&img, 411, 112);
        assert!(p[3] > 200 && p[1] > 120 && p[1] > p[0] && p[1] > p[2], "effects {effects}: {p:?}");
    }
}

/// On the effect path the dither breaks the gradient's 20 pixel bands into short runs.
#[test]
fn dither_breaks_up_gradient_bands() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let img = render(&mut off, "dither", &dither_list(true), 1.0);
    let mut longest = 0;
    for x in [20, 60, 100] {
        let mut run = 1;
        for y in 1..280 {
            run = if rgba(&img, x, y) == rgba(&img, x, y - 1) { run + 1 } else { 1 };
            longest = longest.max(run);
        }
    }
    assert!(longest < 12, "longest run of one colour: {longest} px");
}
