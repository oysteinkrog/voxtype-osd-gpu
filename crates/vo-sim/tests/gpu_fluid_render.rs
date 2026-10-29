//! Ink and smoke (EG-12): six words dictated at scripted onsets seed the fluid behind two lines
//! of text, drawn as ink, smoke and watercolour, with the older line smeared along the flow.
//!
//! GPU test: `just test-gpu fluid_render`. Goldens live in `tests/goldens` and hold the
//! premultiplied bytes as read back; the contact sheet goes to `/var/tmp/claude/vo-golden`.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::fluid::{word_boxes, FluidLayer, InkStyle, InkWord, SETTLE_SECS};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Point, Rect, Size};
use vo_scene::taffy;
use vo_scene::text::{ShapedLayout, TextLayout};
use vo_scene::Scene;
use vo_sim::fluid_render::install_layer;

type El = Element<()>;

const W: f32 = 360.0;
const H: f32 = 180.0;
/// The dictation, two lines; the second is the newest (the reading line).
const LINES: [&str; 2] = ["ink blooms where", "the words land"];
const LINE_X: f32 = 24.0;
const LINE_Y: [f32; 2] = [62.0, 104.0];
const LINE_H: f32 = 30.0;
const SIZE: f32 = 24.0;
/// One onset per word, and the voice level at it.
const ONSETS: [f64; 6] = [0.2, 0.4, 0.6, 0.8, 1.0, 1.2];
const LEVELS: [f32; 6] = [0.5, 0.8, 0.4, 1.0, 0.6, 0.9];

fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

fn style() -> TextStyle {
    TextStyle::new(0, SIZE, LINE_H)
}

/// The words' boxes in the scene, from the same layout the labels draw with.
fn boxes(fonts: &FontRegistry) -> Vec<Rect> {
    let layout = ShapedLayout::new(fonts.clone());
    LINES
        .iter()
        .zip(LINE_Y)
        .flat_map(|(text, y)| {
            let shaped = layout.layout(text, &style(), None);
            word_boxes(text, &shaped, SIZE).into_iter().map(move |r| Rect::new(r.x + LINE_X, r.y + y, r.w, r.h))
        })
        .collect()
}

/// The band of the reading line: the newest word's box, a little taller.
fn band(boxes: &[Rect]) -> (f32, f32) {
    let r = boxes[boxes.len() - 1];
    (r.y - 3.0, r.y + r.h + 3.0)
}

fn layer(fonts: &FontRegistry, style: InkStyle, light: bool, reduced: bool) -> FluidLayer {
    let boxes = boxes(fonts);
    assert_eq!(boxes.len(), ONSETS.len(), "{boxes:?}");
    let (top, bottom) = band(&boxes);
    let words = boxes.iter().zip(ONSETS.iter().zip(LEVELS)).map(|(&rect, (&t, level))| InkWord { t, rect, level });
    let l = FluidLayer::new(Size::new(W, H), Point::ZERO)
        .style(style)
        .light(light)
        .reduced_motion(reduced)
        .smear(0.08)
        .keep_clear(top, bottom)
        .words(words);
    if style == InkStyle::Smoke {
        l.glow(0.8)
    } else {
        l
    }
}

fn view(fluid: Option<&FluidLayer>, light: bool) -> El {
    let (bg, fg) = if light { (0xf4efe4, 0x1a1f26) } else { (0x0e1117, 0xf2f3f5) };
    let mut root = Element::panel("root").size(W, H).fill(Color::hex(bg, 1.0));
    if let Some(f) = fluid {
        root = root.child(f.view("ink"));
    }
    for (i, (text, y)) in LINES.iter().zip(LINE_Y).enumerate() {
        let line = Element::label(format!("l{i}"), *text)
            .text_style(style())
            .color(Color::hex(fg, if i == 1 { 1.0 } else { 0.75 }));
        root = root.child(at(line, LINE_X, y));
    }
    match fluid {
        Some(f) => root.child(f.smear_view("smear")),
        None => root,
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn scene(fonts: &FontRegistry, fluid: Option<&FluidLayer>, light: bool, reduced: bool) -> Scene<()> {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(fluid, light), 0.0);
    scene
}

/// An offscreen renderer with the layer's fluid installed.
fn offscreen(gpu: &support::Gpu, fonts: &FontRegistry, l: &FluidLayer) -> Offscreen {
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    off.renderer.set_light(l.light);
    install_layer(&mut off.renderer, l, 1.0);
    off
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList) -> Image {
    let px = off.render_to_rgba(list, [W as u32, H as u32], 1.0);
    Image { name: name.into(), w: W as u32, h: H as u32, channels: 4, px }
}

fn rows(img: &Image, y0: u32, y1: u32) -> &[u8] {
    &img.px[(y0 * img.w * 4) as usize..(y1 * img.w * 4) as usize]
}

const STYLES: [(&str, InkStyle); 3] =
    [("ink", InkStyle::Ink), ("smoke", InkStyle::Smoke), ("watercolour", InkStyle::Watercolour)];

#[test]
fn fluid_goldens() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut images = Vec::new();
    let cases = STYLES.iter().map(|&(n, s)| (format!("fluid-{n}"), s, false)).chain([
        ("fluid-ink-light".to_string(), InkStyle::Ink, true),
        ("fluid-watercolour-light".to_string(), InkStyle::Watercolour, true),
    ]);
    for (name, style, light) in cases {
        let l = layer(&fonts, style, light, false);
        let mut off = offscreen(&gpu, &fonts, &l);
        images.push(render(&mut off, &name, &scene(&fonts, Some(&l), light, false).sample(2.0)));
    }
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// In every style the reading line's rows are byte for byte those of the same scene with the
/// fluid at opacity 0, and within the dither of the scene with no fluid layer at all; the dye
/// shows elsewhere.
#[test]
fn newest_line_is_as_without_the_fluid() {
    let fonts = fonts();
    let gpu = support::gpu();
    let (top, bottom) = band(&boxes(&fonts));
    let (y0, y1) = (top.ceil() as u32, bottom.floor() as u32);
    for (name, style) in STYLES {
        let wet_layer = layer(&fonts, style, false, false);
        let dry_layer = wet_layer.clone().opacity(0.0);
        let mut off = offscreen(&gpu, &fonts, &wet_layer);
        let wet = render(&mut off, "wet", &scene(&fonts, Some(&wet_layer), false, false).sample(2.0));
        let dry = render(&mut off, "dry", &scene(&fonts, Some(&dry_layer), false, false).sample(2.0));
        let plain = render(&mut off, "plain", &scene(&fonts, None, false, false).sample(2.0));
        assert!(rows(&wet, y0, y1).iter().any(|&b| b > 200), "{name}: the newest line drew nothing");
        assert!(rows(&wet, y0, y1) == rows(&dry, y0, y1), "{name}: the newest line's pixels changed under the dye");
        let worst = rows(&wet, y0, y1).iter().zip(rows(&plain, y0, y1)).map(|(a, b)| a.abs_diff(*b)).max();
        eprintln!("{name}: newest line against no fluid layer, worst byte off by {worst:?}");
        assert!(worst <= Some(Tolerance::default().per_pixel), "{name}: newest line off by {worst:?}");
        assert!(wet.px != dry.px, "{name}: no dye drawn");
    }
}

/// Frames at equal t are byte-identical: two 60 fps runs, a seek back and forth, and a jump
/// straight to the time.
#[test]
fn fluid_replays_and_seeks() {
    let fonts = fonts();
    let gpu = support::gpu();
    let l = layer(&fonts, InkStyle::Ink, false, false);
    let s = scene(&fonts, Some(&l), false, false);
    let run = || {
        let mut off = offscreen(&gpu, &fonts, &l);
        (0..=150).map(|f| render(&mut off, "f", &s.sample(f as f64 / 60.0)).px).collect::<Vec<_>>()
    };
    let (a, b) = (run(), run());
    let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    assert_eq!(differ, 0, "{differ} of {} frames differ between two 60 fps runs", a.len());
    let mut off = offscreen(&gpu, &fonts, &l);
    let jump = render(&mut off, "jump", &s.sample(2.0));
    render(&mut off, "later", &s.sample(2.4));
    let back = render(&mut off, "back", &s.sample(2.0));
    render(&mut off, "early", &s.sample(0.5));
    let forth = render(&mut off, "forth", &s.sample(2.0));
    assert!(jump.px == a[120], "a jump to 2 s differs from 2 s reached at 60 fps");
    assert!(back.px == a[120], "2 s after a seek back from 2.4 s differs");
    assert!(forth.px == a[120], "2 s after a seek forward from 0.5 s differs");
    assert!(a[90] != a[120], "the ink did not move between 1.5 s and 2 s");
}

/// Reduced motion: dye where the words landed, still: the same bytes at 2 s and 3 s, and the
/// scene asleep after the last word.
#[test]
fn reduced_motion_is_still() {
    let fonts = fonts();
    let gpu = support::gpu();
    for (name, style) in STYLES {
        let l = layer(&fonts, style, false, true);
        let mut off = offscreen(&gpu, &fonts, &l);
        let s = scene(&fonts, Some(&l), false, true);
        let a = render(&mut off, "a", &s.sample(2.0));
        let b = render(&mut off, "b", &s.sample(3.0));
        assert!(a.px == b.px, "{name}: reduced motion frames at 2 s and 3 s differ");
        let none = render(&mut off, "none", &scene(&fonts, None, false, true).sample(2.0));
        assert!(a.px != none.px, "{name}: reduced motion drew no dye");
        assert_eq!(s.next_wake(2.0), None, "{name}: still dye keeps the scene awake");
    }
}

/// The scene stays awake while the fluid moves and sleeps once it has settled.
#[test]
fn fluid_wakes_the_scene() {
    let fonts = fonts();
    let l = layer(&fonts, InkStyle::Ink, false, false);
    let s = scene(&fonts, Some(&l), false, false);
    assert_eq!(s.next_wake(1.0), Some(1.0));
    assert_eq!(s.next_wake(1.32 + SETTLE_SECS + 0.1), None);
}
