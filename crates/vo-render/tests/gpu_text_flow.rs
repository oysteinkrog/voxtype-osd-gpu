//! Text flow on the GPU: centred lines that re-centre as they grow (FX-002), inline objects
//! that take room in live text (FX-063), and glyphs on curves (FX-032, FX-039, FX-040).
//! Each golden stacks the frames of one scenario top to bottom.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use support::{Image, Tolerance};
use vo_core::words::Word;
use vo_render::Offscreen;
use vo_scene::curve::{draw_on_path, path_start, place_on_path, Arrow, Flight};
use vo_scene::element::{Element, TextStyle};
use vo_scene::flow::Revise;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::{Color, Point, Size};
use vo_scene::inline::Inline;
use vo_scene::path::{make, StrokeStyle};
use vo_scene::text::{Align, ShapedLayout, TextLayout};
use vo_scene::{Scene, Transition};

const W: f32 = 360.0;
const H: f32 = 96.0;

fn ink() -> Color {
    Color::hex(0xf2f3f5, 1.0)
}

fn words(list: &[(u32, &str)]) -> Vec<Word> {
    list.iter().map(|&(id, text)| Word { id, text: text.into() }).collect()
}

fn panel(child: Element<()>) -> Element<()> {
    Element::panel("panel").pad(12.0).radius(12.0).fill(Color::hex(0x1d1f24, 1.0)).width(W).height(H).child(child)
}

/// Applies `steps` (time, view) and renders the frames at `times`, stacked.
fn strip(
    off: &mut Offscreen,
    fonts: &FontRegistry,
    name: &str,
    steps: Vec<(f64, Element<()>)>,
    times: &[f64],
) -> Image {
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    let mut px = Vec::new();
    let mut steps = steps.into_iter().peekable();
    for &t in times {
        while let Some((at, view)) = steps.next_if(|s| s.0 <= t) {
            scene.apply(view, at);
        }
        let list = scene.sample(t);
        px.extend(off.render_to_rgba(&list, [W as u32, H as u32], 1.0));
    }
    Image { name: name.into(), w: W as u32, h: H as u32 * times.len() as u32, channels: 4, px }
}

fn centred(w: &[Word]) -> Element<()> {
    panel(
        Element::live_text("live", w)
            .text_style(TextStyle::new(0, 20.0, 26.0).centered().balanced())
            .color(ink())
            .word_enter(Transition::fade())
            .word_arrival(vo_scene::arrival::Arrival::Free)
            .width(W - 24.0),
    )
}

fn inline(w: &[Word], objects: Vec<(u32, Inline)>) -> Element<()> {
    panel(
        Element::live_text("live", w)
            .text_style(TextStyle::new(0, 20.0, 28.0))
            .color(ink())
            .inline_objects(objects)
            .width(W - 24.0),
    )
}

#[test]
fn text_flow_goldens() {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts.clone());
    let frames = [0.9, 1.03, 1.06, 1.1, 1.16, 1.6];

    // A centred line grows by a word and wraps to two balanced lines.
    let grow = strip(
        &mut off,
        &fonts,
        "flow-centre-grow",
        vec![
            (0.0, centred(&words(&[(1, "centred"), (2, "lines"), (3, "stay")]))),
            (
                1.0,
                centred(&words(&[
                    (1, "centred"),
                    (2, "lines"),
                    (3, "stay"),
                    (4, "centred"),
                    (5, "as"),
                    (6, "they"),
                    (7, "grow"),
                ])),
            ),
        ],
        &frames,
    );

    // A pause grows, a gap slot with a waveform takes room, braces and a chip close around words.
    let so = (1, "so");
    let objects = |pause: f32| {
        vec![
            (100, Inline::pause(pause)),
            (101, Inline::gap(1.2, vec![0.3, 0.8, 0.5, 1.0, 0.6, 0.2, 0.7, 0.4])),
            (102, Inline::brace_open().color(Color::hex(0xffb347, 1.0))),
            (103, Inline::brace_close().color(Color::hex(0xffb347, 1.0))),
            (104, Inline::chip_open().color(Color::hex(0x6cb6ff, 1.0))),
            (105, Inline::chip_close().color(Color::hex(0x6cb6ff, 1.0))),
        ]
    };
    let list = |extra: bool| {
        let mut w = vec![Word { id: so.0, text: so.1.into() }, Inline::word(100)];
        w.extend(words(&[(2, "the")]));
        w.push(Inline::word(101));
        if extra {
            w.extend([Inline::word(102)]);
            w.extend(words(&[(3, "plan"), (4, "is")]));
            w.extend([Inline::word(103), Inline::word(104)]);
            w.extend(words(&[(5, "end"), (6, "sentence?")]));
            w.push(Inline::word(105));
        }
        w
    };
    let objects_strip = strip(
        &mut off,
        &fonts,
        "flow-inline",
        vec![(0.0, inline(&list(false), objects(0.2))), (1.0, inline(&list(true), objects(2.0)))],
        &frames,
    );

    // Glyphs on curves, drawn by a canvas: text on a wave, a comma in flight with its leader,
    // and an arrow drawing on.
    let layout = ShapedLayout::new(fonts.clone());
    let line = layout.layout("text on a curve", &TextStyle::new(0, 18.0, 24.0), None);
    let comma = layout.layout(",", &TextStyle::new(0, 20.0, 26.0), None).glyphs[0];
    let curve_view = || {
        let line = line.clone();
        panel(
            Element::canvas("curves", 3.0, move |t, rect, p| {
                let wave = make::wave(rect.x, rect.x + 190.0, rect.y + 40.0, 12.0, 150.0, 0.3);
                p.stroke(&wave, &StrokeStyle::solid(1.0, Color::hex(0x6cb6ff, 0.4)));
                let start = path_start(Align::Center, wave.length(), line.size.w);
                draw_on_path(p, &place_on_path(&line, &wave, start), 18.0, ink());
                let flight = Flight::new(
                    Point::new(rect.x + 330.0, rect.y + 6.0),
                    Point::new(rect.x + 240.0, rect.y + 60.0),
                    1.0,
                    0.4,
                );
                let leader = StrokeStyle::solid(1.2, Color::hex(0x6cb6ff, 0.8));
                flight.draw(p, t, comma.glyph, comma.font, comma.advance, 20.0, ink(), &leader);
                let arrow =
                    Arrow::new(Point::new(rect.x + 210.0, rect.y + 30.0), Point::new(rect.x + 236.0, rect.y + 44.0));
                arrow.draw(
                    p,
                    ((t - 1.0) / 0.5).clamp(0.0, 1.0) as f32,
                    &StrokeStyle::solid(1.4, Color::hex(0xffb347, 1.0)),
                );
            })
            .size(W - 24.0, H - 24.0),
        )
    };
    let curves = strip(&mut off, &fonts, "flow-curves", vec![(0.0, curve_view())], &[0.9, 1.1, 1.2, 1.3, 1.45, 1.6]);

    // Revision looks: a roll swap, and a displaced ghost that floats while its line opens.
    let revised = |revise: Revise| {
        move |w: &[Word]| {
            panel(
                Element::live_text("live", w)
                    .text_style(TextStyle::new(0, 20.0, 26.0))
                    .color(ink())
                    .word_revise(revise)
                    .width(W - 24.0),
            )
        }
    };
    let before = words(&[(1, "meet"), (2, "their"), (3, "team")]);
    let after = words(&[(1, "meet"), (2, "there"), (3, "team")]);
    let roll = strip(
        &mut off,
        &fonts,
        "flow-revise-roll",
        vec![(0.0, revised(Revise::Roll)(&before)), (1.0, revised(Revise::Roll)(&after))],
        &frames,
    );
    let ghost = strip(
        &mut off,
        &fonts,
        "flow-revise-ghost",
        vec![(0.0, revised(Revise::Ghost)(&before)), (1.0, revised(Revise::Ghost)(&after))],
        &[0.9, 1.05, 1.12, 1.3, 2.0, 3.4],
    );

    let failures: Vec<String> = [grow, objects_strip, curves, roll, ghost]
        .iter()
        .filter_map(|img| support::check(img, Tolerance::default()).err())
        .collect();
    assert!(failures.is_empty(), "golden mismatch:\n{}", failures.join("\n"));
}
