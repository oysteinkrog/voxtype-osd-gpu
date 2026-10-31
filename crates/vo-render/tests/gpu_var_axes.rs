//! Smooth variable font axes (EG-23): a word whose weight or width ramps slowly through
//! Roboto Flex grows frame by frame, not in jumps at each quantised instance. The weight
//! between two instances is edge growth and the width a share of the way to the next
//! instance's advances, both measured on the word's own letters, so the switch from one
//! instance to the next does not show.
//!
//! What is left at a switch is the renderer's own: the glyph shader's ink of two neighbouring
//! instances differs from their outlines' area by up to about 1/1000 of a glyph's ink, which is
//! as much as a 4 s ramp adds in one frame. So a switch can show as a frame that adds half a
//! step more or less, and the ramps allow that: a drop of at most 1/1000 of the ink and a
//! largest step of at most 1.6 times the median. Quantised instances alone jump by a whole
//! step, more than ten times the median, which the last check shows.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::var_axes::{breathe, Axes, Breath};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const WORD: &str = "breathing";
/// Text sizes the ramps run at.
const SIZES: [f32; 2] = [40.0, 80.0];
/// The ramp runs from `START` for `SECS`, sampled at 60 fps.
const START: f64 = 1.0;
const SECS: f64 = 4.0;
const FPS: f64 = 60.0;

/// The surface for text at `size`.
fn surface(size: f32) -> Size {
    Size::new(5.0 * size, 2.0 * size)
}

/// The word at `axes`, smooth or in the quantised instance alone.
fn view(fonts: &FontRegistry, base: u32, size: f32, axes: Axes, smooth: bool) -> Element<()> {
    let words = vec![Word { id: 1, text: WORD.into() }];
    let breath = if smooth {
        breathe(fonts, base, WORD, axes).expect("flex")
    } else {
        Breath::plain(fonts.add_instance(base, &axes.quantized().settings()).expect("flex"))
    };
    let s = surface(size);
    let live = Element::live_text("live", &words)
        .text_style(TextStyle::new(base, size, 1.2 * size))
        .color(Color::WHITE)
        .word_breath(vec![(1, breath)])
        .width(s.w - 20.0);
    Element::panel("panel").pad(10.0).width(s.w).height(s.h).child(live)
}

/// Runs a ramp of `axes(k)` for k from 0 to 1 and calls `each` with every frame's display
/// list after the word has settled.
fn ramp(
    fonts: &FontRegistry,
    base: u32,
    (size, smooth): (f32, bool),
    axes: impl Fn(f32) -> Axes,
    mut each: impl FnMut(&vo_scene::display::DisplayList),
) {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, surface(size), 1.0);
    scene.apply(view(fonts, base, size, axes(0.0), smooth), 0.0);
    let frames = (SECS * FPS).round() as usize;
    for n in 0..=frames {
        let t = START + n as f64 / FPS;
        let k = n as f32 / frames as f32;
        scene.apply(view(fonts, base, size, axes(k), smooth), t);
        each(&scene.sample(t));
    }
}

fn flex() -> (FontRegistry, u32) {
    let fonts = FontRegistry::new();
    let base = fonts.asset("flex").unwrap();
    (fonts, base)
}

/// The median and the largest step between frames, and the most it goes down.
fn steps(what: &str, values: &[f32]) -> (f32, f32, f32) {
    let steps: Vec<f32> = values.windows(2).map(|w| w[1] - w[0]).collect();
    let mut sorted = steps.clone();
    sorted.sort_by(f32::total_cmp);
    let (median, largest, drop) = (sorted[sorted.len() / 2], sorted[sorted.len() - 1], -sorted[0].min(0.0));
    eprintln!(
        "{what}: {} to {}, median step {median}, largest {largest} ({:.2}x), drop {drop}",
        values[0],
        values[values.len() - 1],
        largest / median
    );
    (median, largest, drop)
}

/// GPU ink of the word in each frame of a 400 to 600 weight ramp at `size`: the alpha summed
/// over four renders moved by parts of a pixel. A frame adds about 1/500 of a level to each
/// edge pixel, under the readback's 1/255, and stems on one pixel column round alike; the
/// four renders take that rounding out of the steps.
fn weight_ramp_ink(gpu: &support::Gpu, size: f32, smooth: bool) -> (Vec<f32>, usize) {
    let (fonts, base) = flex();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let before = fonts.len();
    let s = surface(size);
    let mut ink = Vec::new();
    ramp(
        &fonts,
        base,
        (size, smooth),
        |k| Axes { wght: 400.0 + 200.0 * k, ..Axes::REST },
        |list| {
            let mut sum = 0.0;
            for (dx, dy) in [(0.0, 0.0), (0.25, 0.5), (0.5, 0.25), (0.75, 0.75)] {
                let mut l = list.clone();
                l.layers[0].transform = vo_scene::geom::Affine::translate(dx, dy).then(&l.layers[0].transform);
                let px = off.render_to_rgba(&l, [s.w as u32, s.h as u32], 1.0);
                sum += px.chunks(4).map(|p| p[3] as f32 / 255.0).sum::<f32>();
            }
            ink.push(0.25 * sum);
        },
    );
    (ink, fonts.len() - before)
}

#[test]
fn a_weight_ramp_thickens_every_frame() {
    let gpu = support::gpu();
    for size in SIZES {
        let (ink, made) = weight_ramp_ink(&gpu, size, true);
        let what = format!("ink at {size} px");
        let (median, largest, drop) = steps(&what, &ink);
        assert!(median > 0.0, "{what}: does not grow");
        assert!(drop <= 1e-3 * ink[0], "{what}: drops by {drop}");
        assert!(largest <= 1.6 * median, "{what}: largest step {largest}, median {median}");
        assert!(made <= 11, "{what}: {made} instances");
        // The quantised instances alone, for comparison: a jump at every step.
        let (plain, _) = weight_ramp_ink(&gpu, size, false);
        let (median, largest, _) = steps(&format!("{what}, quantised alone"), &plain);
        assert!(largest > 10.0 * median.max(1e-3), "{what}: quantised alone, largest {largest}, median {median}");
    }
}

/// The word's width in layout units, as live text lays it out (`layout_fonts_toward`).
#[test]
fn a_width_ramp_widens_every_frame() {
    use vo_scene::text::TextLayout;
    let (fonts, base) = flex();
    let before = fonts.len();
    let layout = ShapedLayout::new(fonts.clone());
    let style = TextStyle::new(base, SIZES[0], 1.2 * SIZES[0]);
    let frames = (SECS * FPS).round() as usize;
    let widths: Vec<f32> = (0..=frames)
        .map(|n| {
            let axes = Axes { wdth: 95.0 + 10.0 * n as f32 / frames as f32, ..Axes::REST };
            let b = breathe(&fonts, base, WORD, axes).unwrap();
            let all = 0..WORD.len();
            let toward: Vec<_> = b.toward.iter().flatten().map(|&(f, k)| (all.clone(), f, k)).collect();
            layout.layout_fonts_toward(WORD, &style, None, &[], &[(all.clone(), b.font)], &toward).size.w
        })
        .collect();
    let (median, largest, drop) = steps("width", &widths);
    assert!(median > 0.0 && drop == 0.0, "width: median {median}, drop {drop}");
    assert!(largest <= 1.5 * median, "width: largest step {largest}, median {median}");
    let made = fonts.len() - before;
    assert!(made <= 11, "{made} instances for the width ramp");
}

/// The same ramp through live text: the drawn pens never move back.
#[test]
fn live_text_follows_the_width_ramp() {
    let (fonts, base) = flex();
    let mut widths = Vec::new();
    ramp(
        &fonts,
        base,
        (SIZES[0], true),
        |k| Axes { wdth: 95.0 + 10.0 * k, ..Axes::REST },
        |list| {
            let xs: Vec<f32> = list.glyphs.iter().filter(|g| g.word == 1).map(|g| g.pos.x).collect();
            widths.push(xs.iter().copied().fold(f32::MIN, f32::max) - xs.iter().copied().fold(f32::MAX, f32::min));
        },
    );
    let drops = widths.windows(2).map(|w| w[1] - w[0]).fold(f32::INFINITY, f32::min);
    // Pens sit on a 1/128 px grid.
    assert!(drops >= -1.0 / 128.0, "drops by {drops}");
    assert!(widths[widths.len() - 1] > widths[0] + 2.0, "{} {}", widths[0], widths[widths.len() - 1]);
}
