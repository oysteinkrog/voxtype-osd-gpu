//! Masks (FX04): a layer shows only where its mask draws (alpha), where it does not
//! (inverted), or by the mask's brightness (luma). Built with the scene API, rendered at 1x
//! and 1.5x against goldens, twice for determinism.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{DisplayList, LayerTarget, ShapeKind};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::FontRegistry;
use vo_scene::geom::{Color, Size};
use vo_scene::taffy::prelude::{AlignItems, JustifyContent};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Bold.ttf";

type El = Element<()>;

fn hex(rgb: u32) -> Color {
    Color::hex(rgb, 1.0)
}

/// A gradient bar, for something to mask.
fn bar(key: &'static str) -> El {
    Element::panel(key).size(150.0, 60.0).radius(10.0).fill_gradient(hex(0xff4d6d), hex(0x4cc9f0))
}

/// A 150 by 60 layer holding `content` and a mask.
fn masked(key: &'static str, content: El, mask: El) -> El {
    Element::layer(key).size(150.0, 60.0).child(content).child(mask)
}

fn view() -> El {
    // Alpha: the bar shows through the word.
    let text = masked(
        "text",
        bar("bar"),
        Element::mask("m")
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .child(Element::label("word", "MASK").text_style(TextStyle::new(0, 46.0, 52.0)).color(Color::WHITE)),
    );
    // Inverted: a card with a circle knocked out, its mask listed first.
    let knock = Element::layer("knock")
        .size(150.0, 60.0)
        .child(
            Element::mask("m")
                .invert_mask()
                .align_items(AlignItems::CENTER)
                .justify(JustifyContent::CENTER)
                .child(Element::shape("hole", ShapeKind::Circle).size(36.0, 36.0).fill(Color::WHITE)),
        )
        .child(bar("bar"));
    // Luma: a white to black gradient fades the bar out toward the bottom.
    let luma = masked(
        "luma",
        bar("bar"),
        Element::mask("m")
            .luma_mask()
            .child(Element::panel("ramp").size(150.0, 60.0).fill_gradient(Color::WHITE, Color::BLACK)),
    );
    // An empty alpha mask hides its layer; an empty inverted one hides nothing.
    let empty = masked("empty", bar("bar"), Element::mask("m"));
    let empty_inverted = masked("empty-inv", bar("bar"), Element::mask("m").invert_mask());
    let row = |key: &str, a: El, b: El| Element::row(key.to_string()).gap(12.0).child(a).child(b);
    Element::panel("root")
        .size(336.0, 228.0)
        .fill(hex(0x101420))
        .pad(12.0)
        .gap(12.0)
        .child(row("r1", text, knock))
        .child(row("r2", luma, empty))
        .child(row("r3", empty_inverted, Element::panel("spacer").size(150.0, 60.0)))
}

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(FONT.as_ref()).unwrap_or_else(|e| panic!("{FONT}: {e} (install noto-fonts)"));
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts)));
    scene.set_viewport(0.0, Size::new(336.0, 228.0), 1.0);
    scene.apply(view(), 0.0);
    scene
}

fn offscreen(gpu: &support::Gpu) -> Offscreen {
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let data = std::fs::read(FONT).unwrap();
    assert_eq!(off.renderer.add_font(data).unwrap(), 0);
    off
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList, scale: f32) -> Image {
    let (w, h) = ((list.size.w * scale).ceil() as u32, (list.size.h * scale).ceil() as u32);
    let px = off.render_to_rgba(list, [w, h], scale);
    Image { name: name.into(), w, h, channels: 4, px }
}

#[test]
fn mask_goldens() {
    let gpu = support::gpu();
    let mut off = offscreen(&gpu);
    let list = scene().sample(5.0);
    assert_eq!(list.layers.iter().filter(|l| matches!(l.target, LayerTarget::Mask(_))).count(), 5);
    let mut failures = Vec::new();
    for (name, scale) in [("fx-mask-1x", 1.0), ("fx-mask-1.5x", 1.5)] {
        let img = render(&mut off, name, &list, scale);
        failures.extend(support::check(&img, Tolerance::default()).err());
        let mut other = offscreen(&gpu);
        assert_eq!(render(&mut other, name, &list, scale).px, img.px, "{name} is not deterministic");
    }
    assert!(failures.is_empty(), "{failures:#?}");

    let img = render(&mut off, "mask", &list, 1.0);
    let px = |x: u32, y: u32| {
        let i = ((y * img.w + x) * 4) as usize;
        [img.px[i], img.px[i + 1], img.px[i + 2]]
    };
    let background = px(4, 4);
    // Cells: (12, 12) and (174, 12), then (12, 84) and (174, 84), then (12, 156).
    assert_eq!(px(16, 16), background, "outside the word the bar is masked away");
    assert_eq!(px(174 + 75, 12 + 30), background, "the hole shows the background");
    assert_ne!(px(174 + 10, 12 + 30), background, "the knocked-out card shows around the hole");
    let bright = |p: [u8; 3]| p.iter().map(|&c| c as u32).sum::<u32>();
    assert!(bright(px(12 + 75, 84 + 8)) > 2 * bright(px(12 + 75, 84 + 52)), "luma fades downward");
    assert_eq!(px(174 + 75, 84 + 30), background, "an empty alpha mask hides its layer");
    assert_ne!(px(12 + 75, 156 + 30), background, "an empty inverted mask hides nothing");
}

#[cfg(feature = "cpu")]
#[test]
fn the_cpu_first_frame_hides_alpha_masked_layers() {
    let list = scene().sample(5.0);
    let fonts = FontRegistry::new();
    fonts.add_file(FONT.as_ref()).unwrap();
    let mut cpu = vo_render::cpu::CpuRenderer::new(fonts);
    let (w, h) = (list.size.w as u32, list.size.h as u32);
    let mut got = vec![0u8; (w * h * 4) as usize];
    cpu.draw_bgra(&list, 1.0, w, h, w as usize * 4, &mut got);
    let px = |x: u32, y: u32| {
        let i = ((y * w + x) * 4) as usize;
        [got[i], got[i + 1], got[i + 2]]
    };
    let background = px(4, 4);
    // The alpha-masked bar and the white mask word are not drawn; the bar under an inverted
    // mask is drawn whole.
    assert_eq!(px(12 + 75, 12 + 30), background);
    assert_ne!(px(174 + 75, 12 + 30), background);
}
