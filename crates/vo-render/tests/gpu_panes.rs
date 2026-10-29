//! Many turned panes drawn straight into their surface (EG-33). A turned layer that holds
//! only shapes and glyphs takes no texture of its own: its instances go through the layer's
//! warp in the vertex stage (`common.wgsl` `px_to_ndc`), and fragments map back through the
//! inverse for clips and antialiasing. The same panes forced offscreen must look the same.
//! A turned layer with bloom still goes offscreen.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use vo_render::profile::SurfaceKind;
use vo_render::Offscreen;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::taffy::{AlignItems, JustifyContent};
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

const W: f32 = 600.0;
const H: f32 = 300.0;
const WORDS: [&str; 20] = [
    "so", "the", "plan", "for", "tomorrow", "is", "to", "open", "the", "file", "and", "set", "the", "theme", "then",
    "read", "the", "live", "words", "back",
];

#[derive(Clone, Copy, PartialEq)]
enum Path {
    Direct,
    Offscreen,
    /// The first pane blooms; the rest are direct.
    Bloom,
}

/// 20 word frames in four rows of five, each turned toward the middle and pushed further
/// back than the one before it.
fn view(path: Path, light: bool) -> Element<()> {
    let (bg, fill, edge, ink) = if light {
        (0xf4f6fa, Color::hex(0xffffff, 0.9), 0x2a6fdb, 0x1a2230)
    } else {
        (0x05080e, Color::hex(0x1d5f7a, 0.45), 0x6fe8ff, 0xdffaff)
    };
    let pane = |i: usize| {
        let (col, row) = ((i % 5) as f32, (i / 5) as f32);
        let layer = Element::layer(format!("pane{i}"))
            .translate_z(-40.0 * i as f32)
            .rotate_y(0.35 * (col - 2.0))
            .rotate_x(0.25 * (row - 1.5))
            .child(
                Element::panel(format!("frame{i}"))
                    .size(100.0, 46.0)
                    .radius(9.0)
                    .fill(fill)
                    .stroke(Color::hex(edge, 0.9), 1.5)
                    .align_items(AlignItems::CENTER)
                    .justify(JustifyContent::CENTER)
                    .child(
                        Element::label(format!("word{i}"), WORDS[i])
                            .text_style(TextStyle::new(0, 20.0, 24.0))
                            .color(Color::hex(ink, 1.0)),
                    ),
            );
        match path {
            Path::Offscreen => layer.offscreen(),
            Path::Bloom if i == 0 => layer.bloom(1.0, 12.0),
            _ => layer,
        }
    };
    let row = |r: usize| Element::row(format!("row{r}")).gap(12.0).children((0..5).map(move |c| pane(r * 5 + c)));
    Element::panel("root")
        .size(W, H)
        .fill(Color::hex(bg, 1.0))
        .align_items(AlignItems::CENTER)
        .justify(JustifyContent::CENTER)
        .child(Element::column("grid").gap(14.0).align_items(AlignItems::CENTER).children((0..4).map(row)))
}

fn sample(fonts: &FontRegistry, path: Path, light: bool) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(path, light), 0.0);
    scene.sample(5.0)
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn layer_surfaces(off: &Offscreen) -> usize {
    off.renderer.frame_report().surfaces.iter().filter(|s| s.kind == SurfaceKind::Layer).count()
}

/// Mean and largest channel difference, in 1/255 steps.
fn diff(a: &[u8], b: &[u8]) -> (f64, u8) {
    let (mut sum, mut max) = (0u64, 0u8);
    for (x, y) in a.iter().zip(b) {
        let d = x.abs_diff(*y);
        sum += d as u64;
        max = max.max(d);
    }
    (sum as f64 / a.len() as f64, max)
}

#[test]
fn twenty_turned_panes_draw_without_offscreen_layers() {
    let fonts = fonts();
    let direct = sample(&fonts, Path::Direct, false);
    assert_eq!(direct.layers.iter().filter(|l| l.warp.is_some()).count(), 20, "every pane is turned");

    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    off.render_to_rgba(&direct, [W as u32, H as u32], 1.0);
    let report = off.renderer.frame_report();
    assert_eq!(layer_surfaces(&off), 0, "no pane gets a texture");
    assert!(!report.effects, "the frame stays on the direct path");
    assert!(direct.glyphs.len() >= 40, "every frame holds its word");
    assert_eq!(report.glyphs, direct.glyphs.len());

    let forced = sample(&fonts, Path::Offscreen, false);
    off.render_to_rgba(&forced, [W as u32, H as u32], 1.0);
    assert_eq!(layer_surfaces(&off), 20, "forced offscreen: one texture per pane");
}

#[test]
fn a_turned_pane_with_bloom_still_goes_offscreen() {
    let fonts = fonts();
    let list = sample(&fonts, Path::Bloom, false);
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    off.render_to_rgba(&list, [W as u32, H as u32], 1.0);
    let report = off.renderer.frame_report();
    let layers: Vec<_> = report.surfaces.iter().filter(|s| s.kind == SurfaceKind::Layer).collect();
    assert_eq!(layers.len(), 1, "only the blooming pane is offscreen");
    assert!(layers[0].bloom.is_some());
    assert!(report.effects);
}

/// The direct panes and the same panes drawn offscreen and composited match: mean difference
/// at most 1/255, largest at most 16/255 (edges), at scale 1 and 1.5 and in light mode.
#[test]
fn direct_panes_match_offscreen_panes() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let mut failures = Vec::new();
    for (scale, light) in [(1.0, false), (1.5, false), (1.0, true)] {
        off.renderer.set_light(light);
        let size = [(W * scale) as u32, (H * scale) as u32];
        let a = off.render_to_rgba(&sample(&fonts, Path::Direct, light), size, scale);
        let b = off.render_to_rgba(&sample(&fonts, Path::Offscreen, light), size, scale);
        let (mean, max) = diff(&a, &b);
        eprintln!("panes at {scale}x light={light}: mean {mean:.4}/255, max {max}/255");
        if mean > 1.0 || max > 16 {
            let name = format!("panes-{scale}x-{}", if light { "light" } else { "dark" });
            failures.push(format!("{name}: mean {mean:.3}, max {max}"));
        }
    }
    off.renderer.set_light(false);
    assert!(failures.is_empty(), "direct and offscreen panes differ:\n{}", failures.join("\n"));
}
