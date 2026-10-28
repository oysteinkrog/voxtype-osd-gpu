//! Water shading (EG-10): a pool below three lines of text, with drops at three scripted
//! onsets, drawn by the water material and stepped by its painter.
//!
//! GPU test: `just test-gpu water_render`. Goldens live in `tests/goldens` and hold the
//! premultiplied bytes as read back; the contact sheet goes to `/var/tmp/claude/vo-golden`.

#[path = "../../vo-render/tests/support/mod.rs"]
mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Rect, Size};
use vo_scene::taffy;
use vo_scene::text::ShapedLayout;
use vo_scene::water::{PoolDrop, WaterPool};
use vo_scene::Scene;
use vo_sim::water_render::install_pool;

type El = Element<()>;

const W: f32 = 360.0;
const H: f32 = 180.0;
const LINES: [&str; 3] = ["drops fall where", "the words land and", "the newest line holds"];
const LINE_Y: [f32; 3] = [10.0, 40.0, 70.0];
const LINE_H: f32 = 28.0;
const POOL: Rect = Rect::new(0.0, 112.0, W, H - 112.0);

fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

/// Drops at 0.2, 0.45 and 0.7 s under the words.
fn pool(light: bool, reduced: bool) -> WaterPool {
    WaterPool::new(Size::new(W, H), POOL).light(light).reduced_motion(reduced).drops([
        PoolDrop::word(0.2, 0.2, 0.6),
        PoolDrop::word(0.45, 0.5, 1.0),
        PoolDrop::word(0.7, 0.8, 0.8),
    ])
}

/// What stands in for the pool. `Dry` is the scene without water: a layer that captures its
/// backdrop and draws nothing, so the frame takes the effect path like the pool's and only the
/// water differs.
#[derive(Clone, Copy)]
enum Pool<'a> {
    Wet(&'a WaterPool),
    Dry,
}

fn view(water: Option<Pool<'_>>, light: bool) -> El {
    let (bg, fg) = if light { (0xeef2f6, 0x1a1f26) } else { (0x14161b, 0xf2f3f5) };
    let mut root = Element::panel("root").size(W, H).fill(Color::hex(bg, 1.0));
    for (i, (text, y)) in LINES.iter().zip(LINE_Y).enumerate() {
        let line = Element::label(format!("l{i}"), *text)
            .text_style(TextStyle::new(0, 22.0, LINE_H))
            .color(Color::hex(fg, if i == 2 { 1.0 } else { 0.7 }));
        root = root.child(at(line, 20.0, y));
    }
    match water {
        Some(Pool::Wet(p)) => root.child(p.view("water")),
        Some(Pool::Dry) => root.child(
            at(Element::layer("water").size(W, H), 0.0, 0.0)
                .backdrop(vo_scene::display::BackdropRes::Full, Color::TRANSPARENT),
        ),
        None => root,
    }
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn scene(fonts: &FontRegistry, water: Option<Pool<'_>>, light: bool, reduced: bool) -> Scene<()> {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(water, light), 0.0);
    scene
}

/// An offscreen renderer with the pool's water installed.
fn offscreen(gpu: &support::Gpu, fonts: &FontRegistry, p: &WaterPool, light: bool) -> Offscreen {
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    off.renderer.set_light(light);
    install_pool(&mut off.renderer, p, 1.0);
    off
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList) -> Image {
    let px = off.render_to_rgba(list, [W as u32, H as u32], 1.0);
    Image { name: name.into(), w: W as u32, h: H as u32, channels: 4, px }
}

/// Bytes that differ in rows `y0..y1`, and the largest difference.
fn diff(a: &Image, b: &Image, y0: u32, y1: u32) -> (usize, u8) {
    rows(a, y0, y1)
        .iter()
        .zip(rows(b, y0, y1))
        .filter(|(x, y)| x != y)
        .fold((0, 0), |(n, w), (x, y)| (n + 1, w.max(x.abs_diff(*y))))
}

fn rows(img: &Image, y0: u32, y1: u32) -> &[u8] {
    &img.px[(y0 * img.w * 4) as usize..(y1 * img.w * 4) as usize]
}

#[test]
fn water_goldens() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut images = Vec::new();
    for (name, light) in [("water-pool", false), ("water-pool-light", true)] {
        let p = pool(light, false);
        let mut off = offscreen(&gpu, &fonts, &p, light);
        images.push(render(&mut off, name, &scene(&fonts, Some(Pool::Wet(&p)), light, false).sample(1.0)));
    }
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// The newest line's pixels are the same as in the scene without water (with the same
/// capture, so both frames take the effect path); the pool's pixels are not.
#[test]
fn newest_line_is_as_without_water() {
    let fonts = fonts();
    let gpu = support::gpu();
    let p = pool(false, false);
    let mut off = offscreen(&gpu, &fonts, &p, false);
    let wet = render(&mut off, "wet", &scene(&fonts, Some(Pool::Wet(&p)), false, false).sample(1.0));
    let dry = render(&mut off, "dry", &scene(&fonts, Some(Pool::Dry), false, false).sample(1.0));
    let plain = render(&mut off, "plain", &scene(&fonts, None, false, false).sample(1.0));
    let (n, worst) = diff(&wet, &plain, LINE_Y[2] as u32, (LINE_Y[2] + LINE_H) as u32);
    // Against the direct path (no capture at all) only the effect path's dither differs.
    eprintln!("newest line against the direct path: {n} bytes differ, worst by {worst}");
    assert!(worst <= Tolerance::default().per_pixel, "newest line off by {worst} from the direct path");
    let (y0, y1) = (LINE_Y[2] as u32, (LINE_Y[2] + LINE_H) as u32);
    assert!(rows(&wet, y0, y1).iter().any(|&b| b > 200), "the newest line drew nothing");
    assert!(rows(&wet, y0, y1) == rows(&dry, y0, y1), "the newest line's pixels changed under the water");
    let pool_rows = (POOL.y as u32, H as u32);
    assert!(rows(&wet, pool_rows.0, pool_rows.1) != rows(&dry, pool_rows.0, pool_rows.1), "no pool drawn");
}

/// Reduced motion: a flat pool and a still reflection, the same bytes at 1 s and 2 s.
#[test]
fn reduced_motion_is_flat() {
    let fonts = fonts();
    let gpu = support::gpu();
    let p = pool(false, true);
    let mut off = offscreen(&gpu, &fonts, &p, false);
    let s = scene(&fonts, Some(Pool::Wet(&p)), false, true);
    let a = render(&mut off, "a", &s.sample(1.0));
    let b = render(&mut off, "b", &s.sample(2.0));
    assert!(a.px == b.px, "reduced motion: frames at 1 s and 2 s differ");
    let moving = pool(false, false);
    let mut off2 = offscreen(&gpu, &fonts, &moving, false);
    let m = render(&mut off2, "m", &scene(&fonts, Some(Pool::Wet(&moving)), false, false).sample(1.0));
    assert!(a.px != m.px, "the moving pool looks the same as the flat one");
    assert_eq!(s.next_wake(1.0), None, "a flat pool keeps the scene awake");
}

/// Frames at equal t are byte-identical: two 60 fps runs, a seek back and forth, and a jump
/// straight to the time.
#[test]
fn water_replays_and_seeks() {
    let fonts = fonts();
    let gpu = support::gpu();
    let p = pool(false, false);
    let s = scene(&fonts, Some(Pool::Wet(&p)), false, false);
    let run = || {
        let mut off = offscreen(&gpu, &fonts, &p, false);
        (0..=90).map(|f| render(&mut off, "f", &s.sample(f as f64 / 60.0)).px).collect::<Vec<_>>()
    };
    let (a, b) = (run(), run());
    let differ = a.iter().zip(&b).filter(|(x, y)| x != y).count();
    assert_eq!(differ, 0, "{differ} of {} frames differ between two 60 fps runs", a.len());
    let mut off = offscreen(&gpu, &fonts, &p, false);
    let jump = render(&mut off, "jump", &s.sample(1.0));
    render(&mut off, "later", &s.sample(1.4));
    let back = render(&mut off, "back", &s.sample(1.0));
    render(&mut off, "early", &s.sample(0.3));
    let forth = render(&mut off, "forth", &s.sample(1.0));
    assert!(jump.px == a[60], "a jump to 1 s differs from 1 s reached at 60 fps");
    assert!(back.px == a[60], "1 s after a seek back from 1.4 s differs");
    assert!(forth.px == a[60], "1 s after a seek forward from 0.3 s differs");
    assert!(a[30] != a[60], "the water did not move between 0.5 s and 1 s");
}

/// The scene stays awake while the water moves and sleeps once it has settled.
#[test]
fn water_wakes_the_scene() {
    let fonts = fonts();
    let p = pool(false, false);
    let s = scene(&fonts, Some(Pool::Wet(&p)), false, false);
    assert_eq!(s.next_wake(1.0), Some(1.0));
    assert_eq!(s.next_wake(0.7 + vo_scene::water::SETTLE_SECS + 0.1), None);
}
