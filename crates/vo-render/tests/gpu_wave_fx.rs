//! Waveforms, meters and icons (WP8, WP11, WP14): bars v2, the line trace, ribbon strands,
//! segment meters, the new icons and an imported SVG icon. Each golden is a contact sheet.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::bars::{Anchor, BarColour, BarLayout, Bars, Core, Taper};
use vo_scene::element::Element;
use vo_scene::geom::{Color, Size};
use vo_scene::levels::{Levels, Stops};
use vo_scene::ribbon::Ribbon;
use vo_scene::segments::{SegmentLayout, Segments};
use vo_scene::trace::{Trace, TraceMode};
use vo_scene::Scene;

const BG: u32 = 0x07080c;
const CELL: u32 = 0x10131a;

/// Renders rows of `w` x `h` cells, each holding one element sampled at its own time, and
/// checks the sheet against golden `name`.
fn sheet(name: &str, w: f32, h: f32, rows: Vec<Vec<(Element<()>, f64)>>) -> Image {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0) as f32;
    let (pad, gap) = (8.0, 6.0);
    let size =
        Size::new(2.0 * pad + cols * w + (cols - 1.0).max(0.0) * gap, 2.0 * pad + rows.len() as f32 * (h + gap) - gap);
    // Each cell is its own scene sampled at its own time, composed on the CPU.
    let gpu = support::gpu();
    let mut off = Offscreen::new(&gpu.device, &gpu.queue);
    let [pw, ph] = [size.w.ceil() as u32, size.h.ceil() as u32];
    let bg = [(BG >> 16) as u8, (BG >> 8) as u8, BG as u8, 255];
    let mut px: Vec<u8> = (0..pw * ph).flat_map(|_| bg).collect();
    for (r, row) in rows.into_iter().enumerate() {
        for (c, (el, t)) in row.into_iter().enumerate() {
            let cell = Element::<()>::panel("cell").fill(Color::hex(CELL, 1.0)).radius(6.0).pad(6.0).child(el);
            let mut scene: Scene<()> = Scene::new();
            let cs = Size::new(w, h);
            scene.set_viewport(0.0, cs, 1.0);
            scene.apply(Element::column("root").size(w, h).child(cell.size(w, h)), 0.0);
            let l = scene.sample(t);
            let rgba = off.render_to_rgba(&l, [w as u32, h as u32], 1.0);
            let (x0, y0) = (pad as u32 + c as u32 * (w + gap) as u32, pad as u32 + r as u32 * (h + gap) as u32);
            for y in 0..h as u32 {
                for x in 0..w as u32 {
                    let s = ((y * w as u32 + x) * 4) as usize;
                    let d = (((y0 + y) * pw + x0 + x) * 4) as usize;
                    // Premultiplied over the sheet colour.
                    let a = rgba[s + 3] as u32;
                    for k in 0..3 {
                        px[d + k] = (rgba[s + k] as u32 + px[d + k] as u32 * (255 - a) / 255).min(255) as u8;
                    }
                }
            }
        }
    }
    Image { name: name.into(), w: pw, h: ph, channels: 4, px }
}

/// Speech-like levels at 100 a second: syllables in bursts with short pauses.
fn speech(n: usize) -> Arc<[f32]> {
    (0..n)
        .map(|i| {
            let t = i as f32 / 100.0;
            let syllable = (t * 21.0).sin().abs();
            let phrase = (0.5 + 0.5 * (t * 2.3).sin()).powf(0.7);
            let pause = if (t * 0.9).fract() > 0.82 { 0.05 } else { 1.0 };
            (syllable * phrase * pause * 0.95).clamp(0.0, 1.0)
        })
        .collect()
}

fn levels() -> Levels {
    Levels::new(speech(600), 6.0)
}

fn bars() -> Bars {
    let l = levels();
    Bars { levels: l.levels, newest_at: l.newest_at, ..Bars::default() }
}

const W: f32 = 240.0;
const H: f32 = 72.0;

fn sized(e: Element<()>) -> Element<()> {
    e.size(W - 12.0, H - 12.0)
}

#[test]
fn bars_v2_golden() {
    let orange = Color::hex(0xff5a00, 1.0);
    let heat = Stops::new(&[(0.0, Color::hex(0xff3d00, 1.0)), (0.6, orange), (1.0, Color::hex(0xffc04d, 1.0))]);
    let hot = Bars {
        count: 60,
        slot: 0.03,
        width: 0.5,
        colour: BarColour::ByLevel(heat.clone()),
        core: Some(Core::default()),
        glow: Some((4.0, 0.8)),
        taper: Some(Taper::default()),
        layout: BarLayout::Mirror,
        ..bars()
    };
    let across = Bars {
        colour: BarColour::Across(Stops::new(&[
            (0.0, Color::hex(0x22d3ee, 1.0)),
            (0.5, Color::hex(0x6366f1, 1.0)),
            (1.0, Color::hex(0xd946ef, 1.0)),
        ])),
        ..bars()
    };
    let white_loud = Bars {
        colour: BarColour::ByLevel(Stops::new(&[(0.0, Color::hex(0x7dd3fc, 1.0)), (0.75, Color::WHITE)])),
        ..bars()
    };
    let tails = Bars { count: 48, taper: Some(Taper { span: 0.3, ..Taper::default() }), accent: orange, ..bars() };
    let hair = Bars {
        count: 80,
        hairline: true,
        baseline: Some(Color::hex(0x6b7280, 0.6)),
        accent: Color::hex(0xf43f5e, 1.0),
        rest: Color::hex(0x9ca3af, 0.8),
        colour: BarColour::Centre { span: 0.25, soft: 0.15 },
        ..bars()
    };
    let standing = Bars { anchor: Anchor::Bottom, reflection: 0.4, colour: across.colour.clone(), ..bars() };
    let mirror = Bars { layout: BarLayout::Mirror, accent: Color::hex(0x34d399, 1.0), ..bars() };
    let hot_settle = Bars { settle: Some(6.0), ..hot.clone() };
    let cyan = Color::hex(0x38bdf8, 1.0);
    let ring = Bars {
        count: 64,
        slot: 0.03,
        layout: BarLayout::Radial { inner: 0.45 },
        anchor: Anchor::Bottom,
        accent: cyan,
        glow: Some((3.0, 0.7)),
        core: Some(Core { color: Color::hex(0xe0f7ff, 1.0), ..Core::default() }),
        ..bars()
    };
    let bands: Arc<[f32]> = (0..24)
        .map(|i| (0.9 * (-(i as f32 - 5.0).powi(2) / 40.0).exp() + 0.15 * (i as f32 * 1.7).sin().abs()).min(1.0))
        .collect();
    let spectrum = Bars {
        count: 48,
        bands: Some(bands),
        layout: BarLayout::Radial { inner: 0.5 },
        anchor: Anchor::Bottom,
        accent: Color::hex(0xa78bfa, 1.0),
        glow: Some((3.0, 0.6)),
        ..Bars::default()
    };
    let spectrum_strip = Bars { layout: BarLayout::Mirror, anchor: Anchor::Centre, ..spectrum.clone() };
    let ring_both =
        Bars { anchor: Anchor::Centre, layout: BarLayout::Radial { inner: 0.6 }, core: None, ..ring.clone() };
    let rows = vec![
        // Concept 23: heat colours by level, white-hot core, glow, dotted tails, mirrored.
        vec![(sized(Element::bars("b", hot.clone())), 5.0), (sized(Element::bars("b", hot)), 5.6)],
        // Stops across; loud bars turn white.
        vec![(sized(Element::bars("b", across)), 6.0), (sized(Element::bars("b", white_loud)), 6.0)],
        // Dotted tails; hairlines on a baseline with an accent centre.
        vec![(sized(Element::bars("b", tails)), 6.0), (sized(Element::bars("b", hair)), 6.0)],
        // Standing on a reflection; plain mirror layout.
        vec![(sized(Element::bars("b", standing)), 6.0), (sized(Element::bars("b", mirror)), 6.0)],
        // Settling: the hot strip falls to a dotted line.
        vec![(sized(Element::bars("b", hot_settle.clone())), 6.15), (sized(Element::bars("b", hot_settle)), 6.4)],
        // Concept 02: bars round a ring, growing out; and both ways.
        vec![(sized(Element::bars("b", ring)), 6.0), (sized(Element::bars("b", ring_both)), 6.0)],
        // A spectrum ring from 24 band energies; the same bands as a mirrored strip.
        vec![(sized(Element::bars("b", spectrum.clone())), 0.0), (sized(Element::bars("b", spectrum_strip)), 0.0)],
    ];
    let img = sheet("wave-bars-v2", W, H, rows);
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn trace_and_ribbon_golden() {
    let live = Trace { levels: levels(), ..Trace::default() };
    let green = Color::hex(0x22e57a, 1.0);
    let scope = Trace {
        levels: levels(),
        mode: TraceMode::History { window: 2.5 },
        cycles: 60.0,
        speed: 3.0,
        detail: 0.45,
        floor: 0.03,
        ghosts: 1,
        ghost_phase: 0.25,
        fade: 0.04,
        width: 1.2,
        glow: 3.0,
        color: green,
        core: None,
        horizon: Some(green.fade(0.5)),
        step: 1.0,
        ..Trace::default()
    };
    let settle = Trace { settle: Some(6.0), floor: 0.0, ..live.clone() };
    let ribbon = Ribbon { levels: levels(), ..Ribbon::default() };
    let warm = Ribbon {
        strands: 3,
        left: Stops::two(Color::hex(0xf97316, 1.0), Color::hex(0xfacc15, 1.0)),
        right: Stops::two(Color::hex(0xef4444, 1.0), Color::hex(0xf472b6, 1.0)),
        ..ribbon.clone()
    };
    let ribbon_settle = Ribbon { settle: Some(6.0), floor: 0.0, ..ribbon.clone() };
    let rows = vec![
        // Concept 16: a live trace with ghosts on a fading horizon, at two moments.
        vec![(sized(Element::trace("t", live.clone())), 5.0), (sized(Element::trace("t", live)), 5.5)],
        // Concept 18: a dense oscilloscope over the history; a trace settling flat.
        vec![(sized(Element::trace("t", scope)), 6.0), (sized(Element::trace("t", settle)), 6.2)],
        // Concept 24: ribbon strands, at two moments.
        vec![(sized(Element::ribbon("r", ribbon.clone())), 5.0), (sized(Element::ribbon("r", ribbon)), 5.6)],
        // Three warm strands; strands settling flat.
        vec![(sized(Element::ribbon("r", warm)), 6.0), (sized(Element::ribbon("r", ribbon_settle)), 6.25)],
    ];
    let img = sheet("wave-trace-ribbon", W, H, rows);
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn segment_meters_golden() {
    let cyan = Color::hex(0x38bdf8, 1.0);
    let orange = Color::hex(0xff6a00, 1.0);
    let row = Segments { levels: levels(), cells: 20, ..Segments::default() };
    let partial = Segments { partial: true, ramp: 1.0, peak: None, ..row.clone() };
    let column = Segments {
        cells: 10,
        layout: SegmentLayout::Column,
        on: Stops::two(orange.fade(0.8), orange),
        off: orange.fade(0.12),
        glow: Some((4.0, 0.6)),
        ..row.clone()
    };
    let arc = Segments {
        cells: 30,
        layout: SegmentLayout::Arc { start: 2.3, sweep: 1.6, inner: 0.8, width: 1.6 },
        on: Stops::two(cyan.fade(0.6), cyan),
        off: cyan.fade(0.12),
        clip: None,
        ..row.clone()
    };
    let cell = |m: &Segments, w: f32, h: f32, t: f64| (Element::segments("m", m.clone()).size(w, h), t);
    let rows = vec![
        // LED row with a ramp, peak hold and clip cell, at three moments.
        vec![cell(&row, 200.0, 12.0, 5.0), cell(&row, 200.0, 12.0, 5.3), cell(&partial, 200.0, 12.0, 5.3)],
        // MAX/MIN column with glow; an arc of ticks.
        vec![cell(&column, 14.0, 60.0, 5.0), cell(&column, 14.0, 60.0, 5.3), cell(&arc, 60.0, 60.0, 5.3)],
    ];
    let img = sheet("wave-meters", 220.0, 72.0, rows);
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn icons_and_svg_golden() {
    use vo_scene::font::FontRegistry;
    use vo_scene::geom::Rect;
    use vo_scene::path::icons;
    let fonts = FontRegistry::new();
    let mut set: Vec<(kurbo_path::BezPath, Rect)> = [
        icons::keyboard(),
        icons::chevron(),
        icons::rotated(&icons::chevron(), 0.25),
        icons::clock(),
        icons::bars(),
        icons::lightning(),
        icons::tray(),
        icons::comma(),
        icons::swap(),
        icons::letter_case(),
    ]
    .iter()
    .map(|p| (icons::outline(p, 2.0), Rect::new(0.0, 0.0, 24.0, 24.0)))
    .collect();
    // A line icon in the Lucide style, read from SVG text.
    let svg = r#"<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="M12 2a3 3 0 0 0-3 3v7a3 3 0 0 0 6 0V5a3 3 0 0 0-3-3Z"/>
        <path d="M19 10v2a7 7 0 0 1-14 0v-2"/><line x1="12" x2="12" y1="19" y2="22"/></svg>"#;
    let icon = vo_scene::svg::parse(svg).unwrap();
    set.push((icon.filled(), icon.view));
    let mut root = Element::<()>::row("sheet").pad(8.0).gap(6.0).fill(Color::hex(BG, 1.0));
    for (i, (path, view)) in set.into_iter().enumerate() {
        let id = fonts.add_path(path);
        root = root.child(
            Element::panel(format!("c{i}"))
                .fill(Color::hex(CELL, 1.0))
                .radius(6.0)
                .pad(8.0)
                .child(Element::path("p", id, view).size(32.0, 32.0)),
        );
    }
    let n = 11.0;
    let size = Size::new(16.0 + n * 48.0 + (n - 1.0) * 6.0, 64.0);
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(root, 0.0);
    let list = scene.sample(0.0);
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts);
    let [pw, ph] = [size.w.ceil() as u32, size.h.ceil() as u32];
    let px = off.render_to_rgba(&list, [pw, ph], 1.0);
    let img = Image { name: "wave-icons".into(), w: pw, h: ph, channels: 4, px };
    support::check(&img, Tolerance::default()).unwrap();
}

use vo_scene::path::kurbo as kurbo_path;
