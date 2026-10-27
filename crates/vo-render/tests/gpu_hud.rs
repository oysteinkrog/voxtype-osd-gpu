//! HUD building blocks: effect reach and the surface margin, ring and path repeaters, cut
//! panel frames with stripes, and the radial spectrum.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use std::f32::consts::{PI, TAU};
use std::sync::Arc;
use support::{Image, Tolerance};
use vo_render::Offscreen;

use vo_scene::element::Element;
use vo_scene::geom::{Color, Point, Size};
use vo_scene::path::Contour;
use vo_scene::{surface_for, Corner, Edge, Frame, Insets, Notch, Orient, Scene, Stripes, Ticks};

/// Renders `view` at t on a surface of `size` and returns the premultiplied RGBA bytes.
fn render(view: Element<()>, size: Size, t: f64) -> Image {
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(view, 0.0);
    let list = scene.sample(t);
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let [w, h] = [size.w.ceil() as u32, size.h.ceil() as u32];
    let px = off.render_to_rgba(&list, [w, h], 1.0);
    Image { name: String::new(), w, h, channels: 4, px }
}

/// The largest alpha in the outermost ring of pixels.
fn edge_alpha(img: &Image) -> u8 {
    let (w, h) = (img.w as usize, img.h as usize);
    let a = |x: usize, y: usize| img.px[(y * w + x) * 4 + 3];
    let rows = (0..w).flat_map(|x| [a(x, 0), a(x, h - 1)]);
    let cols = (0..h).flat_map(|y| [a(0, y), a(w - 1, y)]);
    rows.chain(cols).max().unwrap_or(0)
}

/// A toast with a strong glow and a drop shadow, sized 240 x 72.
fn toast() -> Element<()> {
    Element::panel("toast")
        .size(240.0, 72.0)
        .radius(14.0)
        .fill(Color::hex(0x1b1d24, 1.0))
        .stroke(Color::hex(0x7ad7ff, 0.9), 2.0)
        .glow(Color::hex(0x2fb4ff, 1.0), 6.0, 1.4)
        .shadow(Color::hex(0x000000, 0.6), 0.0, 10.0, 8.0)
}

#[test]
fn surface_margin_from_effect_reach_keeps_the_glow() {
    let content = Size::new(240.0, 72.0);
    // Without a margin the glow is cut at the surface edge.
    let tight = render(toast(), content, 0.0);
    assert!(edge_alpha(&tight) > 40, "the glow should reach the edge: {}", edge_alpha(&tight));

    // With the computed margin it fades out inside the surface.
    let reach = toast().effect_reach(0.0);
    // The glow (6 ln(512 x 1.4) = 39.4) reaches further than the shadow (10 + 3 x 8).
    assert!((reach.top - 39.45).abs() < 0.1 && reach.bottom == reach.top, "{reach:?}");
    let fit = surface_for(content, reach);
    let m = Insets::new(
        fit.origin.x,
        fit.origin.y,
        fit.size.w - content.w - fit.origin.x,
        fit.size.h - content.h - fit.origin.y,
    );
    let view = Element::column("margin").pad_edges(m.top, m.right, m.bottom, m.left).child(toast());
    let mut img = render(view.clone(), fit.size, 0.0);
    assert!(edge_alpha(&img) <= 1, "ink at the edge: {}", edge_alpha(&img));

    // The scene's own reach after layout agrees with the element's, and nothing is clipped.
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, fit.size, 1.0);
    scene.apply(view, 0.0);
    assert_eq!(scene.clipped(0.0, fit.size), Insets::ZERO);
    let mut tight_scene: Scene<()> = Scene::new();
    tight_scene.set_viewport(0.0, content, 1.0);
    tight_scene.apply(toast(), 0.0);
    let r = tight_scene.effect_reach(0.0);
    assert!((r.top - reach.top).abs() < 0.5 && (r.bottom - reach.bottom).abs() < 0.5, "{r:?} {reach:?}");

    img.name = "hud-reach-margin".into();
    support::check(&img, Tolerance::default()).unwrap();
}

/// A gauge: 60 ticks inside a ring with every fifth one long and the first 35% lit, and 12
/// hour markers on an inner ring, turned outward.
fn dial() -> Element<()> {
    let cyan = Color::hex(0x35e0ff, 1.0);
    let ticks = Element::polar("ticks", 160.0, 160.0, 74.0, -0.5 * PI, TAU).ticks(
        Ticks::new(60, 6.0, 1.5, Color::hex(0x5a6a80, 1.0))
            .major(5, 12.0, 2.5, Color::hex(0xc8d4e0, 1.0))
            .align(-1.0)
            .lit(0.35, cyan),
    );
    let markers = Element::polar("markers", 160.0, 160.0, 48.0, -0.5 * PI, TAU)
        .orient(Orient::Outward)
        .children((0..12).map(|i| Element::panel(i.to_string()).size(10.0, 3.0).radius(1.5).fill(cyan)));
    Element::layer("dial").size(160.0, 160.0).child(ticks.layout(overlay)).child(markers.layout(overlay))
}

/// Chevrons and ticks along an S-curve.
fn snake() -> Element<()> {
    let pts: Vec<Point> = (0..=48)
        .map(|i| {
            let u = i as f32 / 48.0;
            Point::new(10.0 + 100.0 * u, 80.0 + 50.0 * (u * TAU).sin())
        })
        .collect();
    let contour = Arc::new(Contour::polyline(&pts, false));
    let rail = Element::along_path("rail", 120.0, 160.0, contour.clone(), 0.0, 1.0).ticks(Ticks::new(
        31,
        8.0,
        1.0,
        Color::hex(0x5a6a80, 1.0),
    ));
    let chevrons = Element::along_path("chevrons", 120.0, 160.0, contour, 0.05, 0.95)
        .orient(Orient::Along)
        .path_offset(-10.0)
        .children(
            (0..7).map(|i| Element::panel(i.to_string()).size(9.0, 4.0).radius(2.0).fill(Color::hex(0xff3d8b, 1.0))),
        );
    Element::layer("snake").size(120.0, 160.0).child(rail.layout(overlay)).child(chevrons.layout(overlay))
}

/// Takes the element out of flow at its parent's top left, so siblings stack.
fn overlay(s: &mut vo_scene::taffy::Style) {
    use vo_scene::taffy::{LengthPercentageAuto, Position};
    s.position = Position::Absolute;
    s.inset.left = LengthPercentageAuto::length(0.0);
    s.inset.top = LengthPercentageAuto::length(0.0);
}

#[test]
fn ring_and_path_repeaters() {
    let size = Size::new(320.0, 180.0);
    let view = Element::row("board").size(size.w, size.h).pad(10.0).gap(20.0).child(dial()).child(snake());
    let mut img = render(view, size, 0.0);
    img.name = "hud-repeat".into();
    support::check(&img, Tolerance::default()).unwrap();
}

/// A cyberpunk panel: uneven chamfers, a slanted notch in the top edge, a faint striped
/// fill, a hatched band inside the edge and a glowing stroke.
fn cyber_panel() -> Element<()> {
    let yellow = Color::hex(0xf5e642, 1.0);
    let frame = Frame::new()
        .corners([Corner::Chamfer(18.0), Corner::Chamfer(6.0), Corner::Chamfer(24.0), Corner::Chamfer(6.0)])
        .notch(Notch::new(Edge::Top, 0.62, 40.0, 6.0).slant(6.0))
        .fill(Color::hex(0x0d0f1a, 0.92))
        .stripes(Stripes::new(-1.05, 10.0, 0.35, Color::hex(0xf5e642, 0.05)))
        .hatch(7.0, Stripes::hatch(-1.05, 5.0, 1.5, Color::hex(0xf5e642, 0.55)))
        .stroke(1.5, yellow)
        .glow(3.0);
    Element::frame("cyber", frame).size(160.0, 110.0)
}

/// A mecha plate: notched corners, tabs on the left edge, two cut-out windows and a thick
/// stroke.
fn mecha_plate() -> Element<()> {
    let steel = Color::hex(0x9fb3c8, 1.0);
    let frame = Frame::new()
        .corners([Corner::Notch(8.0), Corner::Chamfer(14.0), Corner::Notch(8.0), Corner::Round(10.0)])
        .notch(Notch::new(Edge::Left, 0.35, 14.0, -4.0))
        .notch(Notch::new(Edge::Left, 0.65, 14.0, -4.0))
        .notch(Notch::new(Edge::Bottom, 0.5, 30.0, 5.0).slant(5.0))
        .cut_rect(110.0, 20.0, 30.0, 10.0, 3.0)
        .cut_rect(110.0, 36.0, 30.0, 10.0, 3.0)
        .fill(Color::hex(0x1c2430, 1.0))
        .stroke(3.0, steel);
    Element::frame("mecha", frame).size(160.0, 110.0)
}

#[test]
fn cut_panel_frames() {
    let size = Size::new(380.0, 150.0);
    let view = Element::row("board").size(size.w, size.h).pad(20.0).gap(20.0).child(cyber_panel()).child(mecha_plate());
    let mut img = render(view, size, 0.0);
    img.name = "hud-frames".into();
    support::check(&img, Tolerance::default()).unwrap();
}
