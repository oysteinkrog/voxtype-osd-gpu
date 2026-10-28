//! Backdrop capture (EG-01): a layer reads what its surface drew before it.
//!
//! The scene is a striped panel with a card over it whose backdrop is drawn back tinted red,
//! so the stripes show red inside the card's rounded box and unchanged around it. A second
//! card sits in an offscreen layer that draws its own stripes: its backdrop is that layer's
//! drawing, not the target's.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{BackdropRes, DisplayList};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::taffy;
use vo_scene::Scene;

type El = Element<()>;

const W: f32 = 320.0;
const H: f32 = 120.0;

/// `e` placed at `x, y` in its parent, out of the flow.
fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

/// Vertical stripes 8 units wide, white and blue.
fn stripes(key: &'static str, n: u32, h: f32) -> El {
    Element::row(key).children((0..n).map(|i| {
        let c = if i % 2 == 0 { Color::WHITE } else { Color::hex(0x3a86ff, 1.0) };
        Element::panel(i as u64).size(8.0, h).fill(c)
    }))
}

fn card(key: &'static str, res: Option<BackdropRes>) -> El {
    let c = Element::layer(key).size(96.0, 48.0).radius(12.0);
    let c = match res {
        Some(r) => c.backdrop(r, Color::rgba(1.0, 0.0, 0.0, 1.0)),
        None => c,
    };
    // A small mark on top: the card's own content draws over its backdrop.
    c.child(at(Element::panel("mark").size(16.0, 4.0).fill(Color::hex(0x10141c, 1.0)), 40.0, 22.0))
}

/// The striped panel, a card over it, and a card in an offscreen group with stripes of its own.
fn list(res: Option<BackdropRes>, scale: f32) -> DisplayList {
    let group = Element::layer("group")
        .size(120.0, 96.0)
        .offscreen()
        .child(at(stripes("gs", 15, 96.0), 0.0, 0.0))
        .child(at(card("inner", res), 12.0, 24.0));
    let root = Element::panel("root")
        .size(W, H)
        .fill(Color::hex(0x07080c, 1.0))
        .child(at(stripes("s", 20, 80.0), 20.0, 20.0))
        .child(at(card("card", res), 40.0, 36.0))
        .child(at(group, 184.0, 12.0));
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), scale);
    scene.apply(root, 0.0);
    scene.sample(0.5)
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
fn backdrop_goldens() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let images = [
        render(&mut off, "bd-full-1x", &list(Some(BackdropRes::Full), 1.0), 1.0),
        render(&mut off, "bd-full-1.5x", &list(Some(BackdropRes::Full), 1.5), 1.5),
        render(&mut off, "bd-half-1x", &list(Some(BackdropRes::Half), 1.0), 1.0),
    ];
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// Inside the card a white stripe turns red and a blue one black; outside it they stay. The
/// card in the offscreen group reads the group's own stripes.
#[test]
fn backdrop_is_tinted_under_the_card_only() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    for scale in [1.0f32, 1.5] {
        let img = render(&mut off, "probe", &list(Some(BackdropRes::Full), scale), scale);
        let px = |x: f32, y: f32| rgba(&img, (x * scale) as u32, (y * scale) as u32);
        // Stripe i covers x 20 + 8 i to 28 + 8 i; the card covers x 40 to 136, y 36 to 84.
        let (white_in, blue_in) = (px(68.0 + 4.0, 40.0), px(76.0 + 4.0, 40.0));
        assert!(white_in[0] > 240 && white_in[1] < 10 && white_in[2] < 10, "{scale}: white under card {white_in:?}");
        assert!(blue_in[2] < 10 && blue_in[1] < 10, "{scale}: blue under card {blue_in:?}");
        let (white_out, blue_out) = (px(68.0 + 4.0, 26.0), px(76.0 + 4.0, 26.0));
        assert!(white_out.iter().all(|&v| v > 240), "{scale}: white stripe outside {white_out:?}");
        assert!(blue_out[2] > 200, "{scale}: blue stripe outside {blue_out:?}");
        // The rounded corner (radius 12) is left alone.
        let corner = px(41.0, 37.0);
        assert!(corner[1] > 200 || corner[2] > 200, "{scale}: the corner keeps its stripe {corner:?}");
        // The group's card: its box is at x 196 to 292, y 36 to 84 in the target. The group's
        // stripe 2 is white, at x 200 to 208.
        let inner = px(204.0, 60.0);
        assert!(inner[0] > 240 && inner[1] < 10, "{scale}: the group's card reads the group {inner:?}");
    }
}

/// Half resolution is a quarter of the texels and matches full resolution away from edges.
#[test]
fn half_resolution_is_a_quarter_of_the_pixels() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let full = render(&mut off, "full", &list(Some(BackdropRes::Full), 1.0), 1.0);
    let full_sizes = off.renderer.backdrop_sizes();
    let half = render(&mut off, "half", &list(Some(BackdropRes::Half), 1.0), 1.0);
    let half_sizes = off.renderer.backdrop_sizes();
    assert_eq!(full_sizes.len(), 2);
    assert_eq!(half_sizes.len(), 2);
    for (f, h) in full_sizes.iter().zip(&half_sizes) {
        assert_eq!(f[0] * f[1], 4 * h[0] * h[1], "full {f:?}, half {h:?}");
    }
    // Stripe middles agree; only the pixel next to a stripe edge softens.
    for x in [72.0, 80.0, 88.0] {
        let (a, b) = (rgba(&full, x as u32, 50), rgba(&half, x as u32, 50));
        assert!(a.iter().zip(b).all(|(p, q)| p.abs_diff(q) <= 2), "x {x}: {a:?} vs {b:?}");
    }
}

/// Two renders of the same scene at the same time are byte-identical.
#[test]
fn backdrop_renders_are_deterministic() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    for res in [BackdropRes::Full, BackdropRes::Half] {
        let l = list(Some(res), 1.5);
        let a = render(&mut off, "a", &l, 1.5);
        let b = render(&mut off, "b", &l, 1.5);
        assert!(a.px == b.px, "{res:?}: two renders differ");
    }
}

/// Without a backdrop the frame stays on the direct path and takes no backdrop texture.
#[test]
fn no_backdrop_costs_nothing() {
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let l = list(None, 1.0);
    assert!(l.layers.iter().all(|l| l.backdrop.is_none()));
    render(&mut off, "plain", &l, 1.0);
    assert!(off.renderer.backdrop_sizes().is_empty());
    assert!(!off.renderer.last_frame_had_effects());
}
