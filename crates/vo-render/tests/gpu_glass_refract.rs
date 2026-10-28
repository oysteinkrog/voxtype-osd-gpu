//! Refracting glass (EG-05): lenses, prisms, slabs and bubbles bend what the popup drew
//! under them, read from their layer's backdrop (EG-01) through the textured glass pipeline.
//!
//! - A lens disc at magnification 1.5 over a HUD ring of 12 ticks shows the ticks 1.5 times
//!   as far apart as without the lens (measured on readback, within 1 px).
//! - A prism over a white beam with dispersion splits the beam's edges: red and blue land at
//!   least 2 px apart, on opposite sides of green.
//! - A flat profile at magnification 1 and dispersion 0 draws the same bytes as plain glass.
//! - Goldens of the four presets, dark and light.
//!
//! GPU test: `just test-gpu` runs it. Goldens hold the premultiplied bytes as read back.

mod support;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::display::{BackdropRes, DisplayList};
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Size};
use vo_scene::glass::{Glass, GlassKind, LensProfile, Refract};
use vo_scene::taffy;
use vo_scene::text::ShapedLayout;
use vo_scene::Scene;

type El = Element<()>;

const W: f32 = 240.0;
const H: f32 = 200.0;
/// The ring's centre and the radius its ticks sit on.
const CX: f32 = 120.0;
const CY: f32 = 100.0;
const RING: f32 = 36.0;
const TICK: f32 = 5.0;

/// `e` placed at `x, y` in its parent, out of the flow.
fn at(e: El, x: f32, y: f32) -> El {
    e.layout(|s| {
        s.position = taffy::style::Position::Absolute;
        s.inset.left = taffy::style::LengthPercentageAuto::length(x);
        s.inset.top = taffy::style::LengthPercentageAuto::length(y);
    })
}

/// A HUD ring: a thin red circle and 12 white square ticks on it.
fn hud_ring() -> Vec<El> {
    let mut out = vec![at(
        Element::panel("ring").size(2.0 * RING, 2.0 * RING).radius(RING).border(Color::hex(0xff2020, 1.0), 1.0),
        CX - RING,
        CY - RING,
    )];
    for i in 0..12 {
        let a = i as f32 * std::f32::consts::TAU / 12.0;
        let (x, y) = (CX + RING * a.cos(), CY + RING * a.sin());
        out.push(at(
            Element::panel(format!("tick{i}")).size(TICK, TICK).fill(Color::WHITE),
            x - 0.5 * TICK,
            y - 0.5 * TICK,
        ));
    }
    out
}

/// A dark backdrop, `under` on it, and over all of it a layer that captures its backdrop and
/// holds `glass` on a panel of `w` by `h` centred on the ring, with an optional caption.
fn view(under: Vec<El>, glass: Option<Glass>, w: f32, h: f32, caption: Option<(&str, Color)>) -> El {
    let mut root = Element::panel("root").size(W, H).fill(Color::hex(0x0b0d12, 1.0));
    for e in under {
        root = root.child(e);
    }
    let mut fx = Element::layer("fx").size(W, H).backdrop(BackdropRes::Full, Color::TRANSPARENT);
    if let Some(g) = glass {
        let mut panel = Element::panel("glass")
            .size(w, h)
            .radius(16.0)
            .glass(g)
            .align_items(taffy::AlignItems::CENTER)
            .justify(taffy::JustifyContent::CENTER);
        if let Some((text, color)) = caption {
            panel = panel.child(Element::label("cap", text).text_style(TextStyle::new(0, 18.0, 22.0)).color(color));
        }
        fx = fx.child(at(panel, CX - 0.5 * w, CY - 0.5 * h));
    }
    root.child(at(fx, 0.0, 0.0))
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    fonts
}

fn list(fonts: &FontRegistry, v: El, t: f64) -> DisplayList {
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(v, 0.0);
    scene.sample(t)
}

fn render(off: &mut Offscreen, name: &str, list: &DisplayList) -> Image {
    let px = off.render_to_rgba(list, [W as u32, H as u32], 1.0);
    Image { name: name.into(), w: W as u32, h: H as u32, channels: 4, px }
}

fn channel(img: &Image, x: u32, y: u32, c: usize) -> f32 {
    img.px[((y * img.w + x) * 4) as usize + c] as f32
}

/// The 12 tick centres on a ring of radius `r` about the ring centre, from the green channel
/// (the one dispersion leaves where magnification puts it): per tick, the weighted centroid
/// of the pixels above half way between the background and the brightest pixel near it.
fn tick_centres(img: &Image, r: f32) -> Vec<(f32, f32)> {
    (0..12)
        .map(|i| {
            let a = i as f32 * std::f32::consts::TAU / 12.0;
            let (ex, ey) = (CX + r * a.cos(), CY + r * a.sin());
            let win = 0.2 * r;
            let pts: Vec<(u32, u32)> = (((ey - win).floor() as u32)..=((ey + win).ceil() as u32))
                .flat_map(|y| (((ex - win).floor() as u32)..=((ex + win).ceil() as u32)).map(move |x| (x, y)))
                .collect();
            let g: Vec<f32> = pts.iter().map(|&(x, y)| channel(img, x, y, 1)).collect();
            let lo = g.iter().cloned().fold(f32::INFINITY, f32::min);
            let hi = g.iter().cloned().fold(0.0, f32::max);
            assert!(hi - lo > 20.0, "tick {i} at {ex:.0},{ey:.0}: no contrast ({lo}..{hi})");
            let thr = 0.5 * (lo + hi);
            let (mut sx, mut sy, mut sw) = (0.0, 0.0, 0.0);
            for (&(x, y), &v) in pts.iter().zip(&g) {
                if v > thr {
                    let w = v - thr;
                    sx += (x as f32 + 0.5) * w;
                    sy += (y as f32 + 0.5) * w;
                    sw += w;
                }
            }
            (sx / sw, sy / sw)
        })
        .collect()
}

fn mean_spacing(c: &[(f32, f32)]) -> f32 {
    let n = c.len();
    (0..n).map(|i| (c[i].0 - c[(i + 1) % n].0).hypot(c[i].1 - c[(i + 1) % n].1)).sum::<f32>() / n as f32
}

/// Under a lens disc at magnification 1.5 the ticks are 1.5 times as far apart.
#[test]
fn lens_magnifies_the_ring_one_and_a_half_times() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let lens = Glass::lens(GlassKind::Dark);
    assert_eq!(lens.refract.magnify, 1.5);
    // The same glass without refraction: the ticks as they are, under the same tint.
    let plain = Glass { refract: Refract::NONE, ..lens };
    let size = 160.0;
    let bent = render(&mut off, "lens", &list(&fonts, view(hud_ring(), Some(lens), size, size, None), 1.0));
    let flat = render(&mut off, "flat", &list(&fonts, view(hud_ring(), Some(plain), size, size, None), 1.0));
    let outside = mean_spacing(&tick_centres(&flat, RING));
    let under = mean_spacing(&tick_centres(&bent, 1.5 * RING));
    eprintln!("tick spacing: {outside:.2} px plain, {under:.2} px under the lens ({:.3}x)", under / outside);
    assert!((under - 1.5 * outside).abs() <= 1.0, "spacing {under:.2} px under the lens, {outside:.2} px outside");
}

/// A prism over a white beam with dispersion: the beam's edges split by channel.
#[test]
fn prism_splits_the_beam_edges() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    // A horizontal beam through the ring centre; the prism moves what is under it down.
    let beam = vec![at(Element::panel("beam").size(W, 8.0).fill(Color::WHITE), 0.0, CY - 4.0)];
    let prism = Glass::prism(GlassKind::Dark).with_refract(Refract::prism(std::f32::consts::FRAC_PI_2, 12.0));
    let img = render(&mut off, "prism", &list(&fonts, view(beam, Some(prism), 170.0, 150.0, None), 1.0));
    // The beam's top edge in column x per channel: where it first crosses half its peak,
    // searched upward from the brightest row (sub-pixel by linear interpolation). Rows 30 px
    // round the beam, clear of the prism's rim light.
    let x = CX as u32;
    let y0 = (CY - 30.0) as u32;
    let edge = |c: usize| -> f32 {
        let col: Vec<f32> = (y0..(CY + 30.0) as u32).map(|y| channel(&img, x, y, c)).collect();
        let (peak_y, &peak) = col.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap();
        let base = col[..peak_y].iter().cloned().fold(f32::INFINITY, f32::min);
        let half = 0.5 * (base + peak);
        let mut y = peak_y;
        while y > 0 && col[y - 1] > half {
            y -= 1;
        }
        // Between rows y - 1 (below half) and y (above).
        let (a, b) = (col[y - 1], col[y]);
        y0 as f32 + (y as f32 - 1.0) + 0.5 + (half - a) / (b - a).max(1e-3)
    };
    let (r, g, b) = (edge(0), edge(1), edge(2));
    eprintln!("beam top edge under the prism: red {r:.2}, green {g:.2}, blue {b:.2}");
    assert!((r - b).abs() >= 2.0, "red and blue edges only {:.2} px apart", (r - b).abs());
    assert!((r - g) * (b - g) < 0.0, "red ({r:.2}) and blue ({b:.2}) are on the same side of green ({g:.2})");
}

/// A flat profile, magnification 1 and dispersion 0 draw exactly what plain glass draws over
/// the same backdrop.
#[test]
fn neutral_refraction_is_plain_glass() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let neutral = Refract { profile: LensProfile::Flat, magnify: 1.0, dispersion: 0.0, ..Refract::lens(1.0) };
    assert!(!neutral.bends());
    for kind in [GlassKind::Dark, GlassKind::Light] {
        let plain = Glass::of_kind(kind).with_shape(vo_scene::glass::GlassShape::Ellipse);
        let a = render(
            &mut off,
            "neutral",
            &list(&fonts, view(hud_ring(), Some(plain.with_refract(neutral)), 160.0, 160.0, None), 1.0),
        );
        let b = render(&mut off, "plain", &list(&fonts, view(hud_ring(), Some(plain), 160.0, 160.0, None), 1.0));
        assert!(a.px == b.px, "{kind:?}: neutral refraction differs from plain glass");
    }
}

/// The four presets, dark and light, each with a caption on it.
fn presets(kind: GlassKind) -> [(&'static str, Glass, f32, f32); 4] {
    [
        ("lens", Glass::lens(kind), 150.0, 150.0),
        ("prism", Glass::prism(kind), 170.0, 150.0),
        ("slab", Glass::slab(kind), 200.0, 110.0),
        ("bubble", Glass::bubble(kind), 150.0, 150.0),
    ]
}

/// Each preset keeps its text at the AX01 bar the glass tests use (4.5:1, light margin 0.02).
#[test]
fn presets_pass_contrast_in_both_modes() {
    for kind in [GlassKind::Dark, GlassKind::Light] {
        for (name, g, _, _) in presets(kind) {
            for text in Glass::preset_texts(kind) {
                let c = g.contrast(text, 0.02);
                assert!(c >= 4.5, "{name} {kind:?}: {text:?} at {c:.2}:1");
            }
            assert!(g.refract.bends(), "{name} bends nothing");
        }
    }
}

#[test]
fn refract_goldens() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let mut images = Vec::new();
    for kind in [GlassKind::Dark, GlassKind::Light] {
        let mode = if kind == GlassKind::Dark { "dark" } else { "light" };
        off.renderer.set_light(kind == GlassKind::Light);
        let text = Glass::preset_texts(kind)[0];
        for (name, g, w, h) in presets(kind) {
            let mut under = hud_ring();
            under.push(at(Element::panel("beam").size(W, 8.0).fill(Color::hex(0x6cb6ff, 1.0)), 0.0, CY + 50.0));
            let l = list(&fonts, view(under, Some(g), w, h, Some(("Aa", text))), 1.0);
            images.push(render(&mut off, &format!("refract-{name}-{mode}"), &l));
        }
    }
    off.renderer.set_light(false);
    let failures: Vec<String> =
        images.iter().filter_map(|img| support::check(img, Tolerance::default()).err()).collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}

/// The bubble's wobble is a pure function of time, and reduced motion keeps its bend still.
#[test]
fn bubble_wobble_replays_and_stops_under_reduced_motion() {
    let fonts = fonts();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let v = || view(hud_ring(), Some(Glass::bubble(GlassKind::Dark)), 150.0, 150.0, None);
    let a = render(&mut off, "a", &list(&fonts, v(), 1.0));
    let b = render(&mut off, "b", &list(&fonts, v(), 1.0));
    assert!(a.px == b.px, "equal t, different bytes");
    let later = render(&mut off, "c", &list(&fonts, v(), 2.0));
    assert!(a.px != later.px, "the wobble does not move");
    let reduced = |t: f64| {
        let mut scene: Scene<()> =
            Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone()))).with_reduced_motion(true);
        scene.set_viewport(0.0, Size::new(W, H), 1.0);
        scene.apply(v(), 0.0);
        let l = scene.sample(t);
        assert_eq!(scene.next_wake(t), None, "reduced motion still asks for frames");
        l
    };
    let (r1, r2) = (reduced(1.0), reduced(2.0));
    assert!(r1.glass[0].glass.refract.bends(), "reduced motion dropped the bend");
    let (i1, i2) = (render(&mut off, "r1", &r1), render(&mut off, "r2", &r2));
    assert!(i1.px == i2.px, "reduced motion: the bubble still moves");
}
