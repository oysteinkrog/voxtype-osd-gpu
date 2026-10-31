//! A word half written in wet ink (SW-03), at scale 1 and 1.5: the pen on each glyph's own
//! centre lines at its stem width, the text colour lifted toward white, the first letters
//! already drying, the nib light in the accent, over the type at its ink floor, in Inter
//! and Saira. A loud and a quiet word written with the nib (SW-05) differ in stroke width.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Point, Size};
use vo_scene::glyph_morph::WordOutline;
use vo_scene::stroke_write::{PenParams, PenSchedule, StrokeSource, WetInk, Writing};
use vo_scene::text::{ShapedLayout, TextLayout};
use vo_scene::Scene;

const W: f32 = 300.0;
const H: f32 = 70.0;

fn half_written(fonts: &FontRegistry, font: u32) -> vo_scene::display::DisplayList {
    let text = "writing";
    let shaped = ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(font, 44.0, 52.0), None);
    let word = WordOutline::new(fonts, &shaped, 44.0, Point::new(12.0, 6.0));
    let writing = Arc::new(Writing::with_source(fonts, text, word, 6.6, StrokeSource::FontNow));
    // A slow pen, so the first letters have dried a little by the middle of the word.
    let params = PenParams { speed: 1500.0, min_secs: 0.5, max_secs: 0.5, ..PenParams::default() };
    let slot = PenSchedule::plan(params, &[(0.0, writing.length())]).slots()[0];
    let text_color = Color::hex(0xd8dbe0, 1.0);
    let wet = WetInk { type_ink: text_color, ..WetInk::new(text_color, Color::hex(0xf59e0b, 1.0)) };
    let t = 0.5 * (slot.start + slot.end);
    let w = writing.clone();
    let view = Element::<()>::panel("bg")
        .size(W, H)
        .fill(Color::hex(0x0e1117, 1.0))
        .child(Element::canvas("pen", 0.0, move |_, _, p| w.draw_wet(p, t, &slot, &wet)).size(W, H));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view, 0.0);
    scene.sample(0.0)
}

fn render(off: &mut Offscreen, name: &str, list: &vo_scene::display::DisplayList, scale: f32) -> Image {
    let [w, h] = [(list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32];
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

#[test]
fn half_written_wet_ink_goldens() {
    let gpu = support::gpu();
    let fonts = FontRegistry::new();
    let inter = fonts.asset("grotesk").unwrap();
    let saira = fonts.asset("extra-condensed-bold").unwrap();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let (a, b) = (half_written(&fonts, inter), half_written(&fonts, saira));
    let images = [
        render(&mut off, "wet-ink-inter-1x", &a, 1.0),
        render(&mut off, "wet-ink-inter-1.5x", &a, 1.5),
        render(&mut off, "wet-ink-saira-1x", &b, 1.0),
        render(&mut off, "wet-ink-saira-1.5x", &b, 1.5),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// "writing" fully written and still wet, at `voice`, with no type and no shine, so only the
/// wet strokes and their pools draw.
fn written(fonts: &FontRegistry, font: u32, voice: f32) -> vo_scene::display::DisplayList {
    let text = "writing";
    let shaped = ShapedLayout::new(fonts.clone()).layout(text, &TextStyle::new(font, 44.0, 52.0), None);
    let word = WordOutline::new(fonts, &shaped, 44.0, Point::new(12.0, 6.0));
    let writing = Arc::new(Writing::with_source(fonts, text, word, 6.6, StrokeSource::FontNow));
    let slot = PenSchedule::plan(PenParams::default(), &[(0.0, writing.length())]).slots()[0];
    let wet = WetInk { sheen: 0.0, nib: 0.0, ..WetInk::new(Color::hex(0xffffff, 1.0), Color::hex(0xf59e0b, 1.0)) };
    let t = slot.end;
    let w = writing.clone();
    let view = Element::<()>::panel("bg")
        .size(W, H)
        .fill(Color::hex(0x000000, 1.0))
        .child(Element::canvas("pen", 0.0, move |_, _, p| w.draw_wet_voiced(p, t, &slot, &wet, voice)).size(W, H));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view, 0.0);
    scene.sample(0.0)
}

/// The ink in an image: the sum of its red channel over 255, a pixel count weighted by
/// coverage. Over the same strokes it grows with their mean width.
fn ink(img: &Image) -> f64 {
    img.px.chunks(4).map(|p| p[0] as f64 / 255.0).sum()
}

#[test]
fn a_loud_word_writes_at_least_10_percent_wider_than_a_quiet_one() {
    let gpu = support::gpu();
    let fonts = FontRegistry::new();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    for face in ["grotesk", "extra-condensed-bold"] {
        let font = fonts.asset(face).unwrap();
        let loud = render(&mut off, "loud", &written(&fonts, font, 1.0), 1.0);
        let quiet = render(&mut off, "quiet", &written(&fonts, font, 0.0), 1.0);
        let (a, b) = (ink(&loud), ink(&quiet));
        eprintln!("{face}: loud {a:.0} quiet {b:.0} ratio {:.3}", a / b);
        assert!(a >= 1.10 * b, "{face}: loud {a:.0} against quiet {b:.0}");
        // The same scenario renders the same twice.
        let again = render(&mut off, "loud", &written(&fonts, font, 1.0), 1.0);
        assert!(again.px == loud.px, "{face}: a second render differs");
    }
}
