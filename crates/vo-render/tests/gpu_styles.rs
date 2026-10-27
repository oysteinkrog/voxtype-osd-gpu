//! Layer styles (FX13): chromatic aberration, glitch slices, scanlines, CRT curvature,
//! vignette and flicker, each on a small card, then all of them on one. Built with the scene
//! API, rendered at 1x and 1.5x against goldens, twice for determinism.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Size};
use vo_scene::taffy::prelude::{AlignItems, JustifyContent};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Bold.ttf";
const W: f32 = 498.0;
const H: f32 = 182.0;

type El = Element<()>;

/// A 150 by 70 card with a word, inside a layer that `style` styles.
fn card(key: &'static str, word: &'static str, style: impl FnOnce(El) -> El) -> El {
    let inner = Element::panel("panel")
        .size(150.0, 70.0)
        .radius(8.0)
        .fill_gradient(Color::hex(0x1d3557, 1.0), Color::hex(0x0b132b, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::label("word", word).text_style(TextStyle::new(0, 26.0, 30.0)).color(Color::hex(0xe9f5ff, 1.0)));
    style(Element::layer(key).size(150.0, 70.0).child(inner))
}

fn view() -> El {
    let row = |key: &'static str, cards: [El; 3]| Element::row(key).gap(12.0).children(cards);
    Element::panel("root")
        .size(W, H)
        .fill(Color::hex(0x07080c, 1.0))
        .pad(12.0)
        .gap(18.0)
        .child(row(
            "r1",
            [
                card("chroma", "chroma", |e| e.chromatic(4.0)),
                card("glitch", "glitch", |e| e.glitch(0.8)),
                card("scan", "scanlines", |e| e.scanlines(0.5, 3.0)),
            ],
        ))
        .child(row(
            "r2",
            [
                card("crt", "curved", |e| e.crt(0.2)),
                card("vignette", "vignette", |e| e.vignette(0.9)),
                card("retro", "retro", |e| e.chromatic(2.0).scanlines(0.35, 3.0).crt(0.12).vignette(0.6).flicker(0.15)),
            ],
        ))
}

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(FONT.as_ref()).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"));
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts)));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(), 0.0);
    scene
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

#[test]
fn style_goldens() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    // A fixed time, so the glitch pattern and the flicker level are fixed too.
    let list = scene().sample(5.2);
    assert_eq!(list.layers.iter().filter(|l| l.style.is_some()).count(), 6);
    let mut failures = Vec::new();
    for (name, scale) in [("fx-style-1x", 1.0), ("fx-style-1.5x", 1.5)] {
        let img = render(&mut off, name, &list, scale);
        failures.extend(support::check(&img, Tolerance::default()).err());
        let mut other = offscreen(&gpu);
        assert_eq!(render(&mut other, name, &list, scale).px, img.px, "{name} is not deterministic");
    }
    assert!(failures.is_empty(), "{failures:#?}");
    // The curved card's corners are empty: the background shows there.
    let img = render(&mut off, "style", &list, 1.0);
    let px = |x: u32, y: u32| {
        let i = ((y * img.w + x) * 4) as usize;
        [img.px[i], img.px[i + 1], img.px[i + 2]]
    };
    assert_eq!(px(12 + 2, 100 + 2), px(4, 4), "the curved card's corner is empty");
    assert_ne!(px(12 + 20, 100 + 35), px(4, 4), "inside, the card shows");
}

#[test]
fn glitch_changes_with_its_step_and_stops_when_settled() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let s = scene();
    let a = render(&mut off, "a", &s.sample(5.2), 1.0).px;
    let b = render(&mut off, "b", &s.sample(5.2 + 1.0 / 15.0), 1.0).px;
    assert_ne!(a, b, "a new glitch step draws a new pattern");
    // The glitch and flicker wake the scene at their own rates: 15 and 6 times a second.
    let next = s.next_wake(5.2).expect("a glitching scene wakes");
    assert!(next > 5.2 && next <= 5.2 + 1.0 / 15.0 + 1e-9, "{next}");
}
