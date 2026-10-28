//! Palimpsest correction (MO-20), the default revision, on the GPU: "there" becomes
//! "their" and a new word arrives after it. The frames run top to bottom: before, the first
//! frame of the change, the ghost sinking, held, dissolving, and settled.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 360.0;
const H: f32 = 84.0;

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn view(w: &[Word]) -> Element<()> {
    let live = Element::live_text("live", w)
        .text_style(TextStyle::new(0, 28.0, 34.0))
        .color(Color::hex(0xf2f3f5, 1.0))
        .width(W - 32.0);
    Element::panel("panel").pad(16.0).radius(12.0).fill(Color::hex(0x1d1f24, 1.0)).width(W).height(H).child(live)
}

fn strip(off: &mut Offscreen, fonts: &FontRegistry, name: &str, reduced: bool) -> Image {
    let steps = [
        (0.0, words(&[(1, "meet"), (2, "there"), (3, "at"), (4, "noon")])),
        (1.0, words(&[(1, "meet"), (2, "their"), (5, "team"), (3, "at"), (4, "noon")])),
    ];
    let times = [0.9, 1.0, 1.04, 1.3, 1.62, 2.4];
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(reduced);
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    let mut px = Vec::new();
    let mut next = 0;
    for &t in &times {
        while next < steps.len() && steps[next].0 <= t {
            scene.apply(view(&steps[next].1), steps[next].0);
            next += 1;
        }
        px.extend(off.render_to_rgba(&scene.sample(t), [W as u32, H as u32], 1.0));
    }
    Image { name: name.into(), w: W as u32, h: H as u32 * times.len() as u32, channels: 4, px }
}

#[test]
fn palimpsest_golden() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let failures: Vec<String> =
        [strip(&mut off, &fonts, "live-palimpsest", false), strip(&mut off, &fonts, "live-palimpsest-reduced", true)]
            .iter()
            .filter_map(|img| support::check(img, Tolerance::default()).err())
            .collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}
