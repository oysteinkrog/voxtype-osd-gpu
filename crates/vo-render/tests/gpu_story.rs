//! Vector content animation: the status icon story, and the waveform, container, progress,
//! frame and erase motions. Each golden is a contact sheet: rows of frames of one motion at
//! chosen times, drawn through canvas elements.
//!
//! GPU test: `just test-gpu` runs it.

mod support;

use std::sync::Arc;

use support::{Image, Tolerance};
use vo_render::Offscreen;
use vo_scene::canvas::Painter;
use vo_scene::element::Element;
use vo_scene::geom::{Color, Point, Rect, Size};
use vo_scene::status_icon::{IconStyle, StatusIcon};
use vo_scene::Scene;

/// One frame: draws into its box.
type Frame = Arc<dyn Fn(Rect, &mut Painter) + Send + Sync>;

const BG: u32 = 0x111318;
const CELL: u32 = 0x181b22;

/// Renders rows of `w` x `h` frames on a dark sheet and checks it against golden `name`.
fn sheet(name: &str, w: f32, h: f32, rows: Vec<Vec<Frame>>) -> Image {
    sheet_with(name, w, h, rows, None)
}

/// [`sheet`] with fonts for frames that draw glyphs.
fn sheet_with(name: &str, w: f32, h: f32, rows: Vec<Vec<Frame>>, fonts: Option<vo_scene::font::FontRegistry>) -> Image {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0) as f32;
    let (pad, gap) = (8.0, 6.0);
    let size =
        Size::new(2.0 * pad + cols * w + (cols - 1.0).max(0.0) * gap, 2.0 * pad + rows.len() as f32 * (h + gap) - gap);
    let mut root = Element::<()>::panel("sheet").pad(pad).gap(gap).fill(Color::hex(BG, 1.0));
    for (r, row) in rows.into_iter().enumerate() {
        let mut line = Element::row(format!("row{r}")).gap(gap);
        for (c, frame) in row.into_iter().enumerate() {
            let cell = Element::canvas(format!("f{c}"), 0.0, move |_t, rect, p: &mut Painter| frame(rect, p));
            line = line
                .child(Element::panel(format!("c{c}")).fill(Color::hex(CELL, 1.0)).radius(6.0).child(cell.size(w, h)));
        }
        root = root.child(line);
    }
    let mut scene: Scene<()> = Scene::new();
    scene.set_viewport(0.0, size, 1.0);
    scene.apply(root, 0.0);
    let list = scene.sample(0.0);
    let gpu = support::gpu();
    let mut off = match fonts {
        Some(f) => Offscreen::with_fonts(&gpu.device, &gpu.queue, f),
        None => Offscreen::new(&gpu.device, &gpu.queue),
    };
    let [pw, ph] = [size.w.ceil() as u32, size.h.ceil() as u32];
    let px = off.render_to_rgba(&list, [pw, ph], 1.0);
    Image { name: name.into(), w: pw, h: ph, channels: 4, px }
}

fn icon_frames(icon: Arc<StatusIcon>, times: &[f64]) -> Vec<Frame> {
    times
        .iter()
        .map(|&t| {
            let icon = icon.clone();
            Arc::new(move |r: Rect, p: &mut Painter| {
                icon.paint(t, Point::new(r.x + 0.5 * r.w, r.y + 0.5 * r.h), r.w.min(r.h), p)
            }) as Frame
        })
        .collect()
}

#[test]
fn status_icon_story_golden() {
    let mut main = StatusIcon::new(IconStyle::default());
    main.record(0.0);
    main.stop(1.0);
    main.finish(1.3);
    main.done(2.77);
    let mut cancel = StatusIcon::new(IconStyle::default());
    cancel.record(0.0);
    cancel.cancel(0.8);
    let rows = vec![
        // Pop, dot, dot to square, square, rounding, ring opening, spinning, spinning.
        icon_frames(Arc::new(main.clone()), &[0.06, 0.6, 1.06, 1.3, 1.38, 1.47, 1.8, 2.4]),
        // Done mid-spin: the arc closes forwards, then the check draws on.
        icon_frames(Arc::new(main), &[2.77, 2.85, 2.95, 3.05, 3.15, 3.25, 3.35, 3.8]),
        // Cancel: the dot shrinks while the X draws on.
        icon_frames(Arc::new(cancel), &[0.8, 0.86, 0.92, 0.98, 1.05, 1.15, 1.3, 1.6]),
    ];
    let img = sheet("story-icon", 64.0, 64.0, rows);
    support::check(&img, Tolerance::default()).unwrap();
}

/// Speech-like levels at 100 a second: syllables in bursts with short pauses.
fn speech(n: usize) -> Vec<f32> {
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

fn bar_frames(bars: Arc<vo_scene::bars::Bars>, times: &[f64]) -> Vec<Frame> {
    times
        .iter()
        .map(|&t| {
            let bars = bars.clone();
            Arc::new(move |r: Rect, p: &mut Painter| {
                let mut v = Vec::new();
                bars.shapes(Rect::new(r.x + 8.0, r.y + 4.0, r.w - 16.0, r.h - 8.0), t, 1.0, &mut v);
                for s in v {
                    p.shape(s);
                }
            }) as Frame
        })
        .collect()
}

#[test]
fn bars_golden() {
    use vo_scene::bars::Bars;
    let levels: Arc<[f32]> = Arc::from(speech(600).as_slice());
    let accent = Color::hex(0x7dd3fc, 1.0);
    let live = Bars { levels: levels.clone(), newest_at: 6.0, accent, ..Bars::default() };
    let settle = Bars { settle: Some(6.0), ..live.clone() };
    let played = |p: f32| Arc::new(Bars { playhead: Some(p), ..live.clone() });
    let rows = vec![
        // Live: the strip scrolls left, bars rise fast and fall slowly.
        bar_frames(Arc::new(live.clone()), &[4.0, 4.5, 5.0, 5.5, 6.0]),
        // Finishing: heights fall to a dotted line over 400 ms.
        bar_frames(Arc::new(settle), &[6.0, 6.1, 6.2, 6.3, 6.4]),
        // A playhead: accent on the left, grey on the right, split inside a bar.
        [0.1, 0.33, 0.5, 0.71, 0.95].iter().flat_map(|&p| bar_frames(played(p), &[6.0])).collect(),
    ];
    let img = sheet("story-bars", 200.0, 48.0, rows);
    support::check(&img, Tolerance::default()).unwrap();
}

const FONT: &str = "/usr/share/fonts/noto/NotoSans-Regular.ttf";

/// Lays `frames` (RGBA, `w` x `h` each) out in rows on the sheet colour.
fn grid(name: &str, w: u32, h: u32, rows: Vec<Vec<Vec<u8>>>) -> Image {
    let (pad, gap) = (8u32, 6u32);
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0) as u32;
    let (gw, gh) = (2 * pad + cols * w + cols.saturating_sub(1) * gap, 2 * pad + rows.len() as u32 * (h + gap) - gap);
    let bg = [(BG >> 16) as u8, (BG >> 8) as u8, BG as u8, 255];
    let mut px: Vec<u8> = (0..gw * gh).flat_map(|_| bg).collect();
    for (r, row) in rows.iter().enumerate() {
        for (c, f) in row.iter().enumerate() {
            let (x0, y0) = (pad + c as u32 * (w + gap), pad + r as u32 * (h + gap));
            for y in 0..h {
                let src = &f[(y * w * 4) as usize..((y + 1) * w * 4) as usize];
                let d = (((y0 + y) * gw + x0) * 4) as usize;
                px[d..d + src.len()].copy_from_slice(src);
            }
        }
    }
    Image { name: name.into(), w: gw, h: gh, channels: 4, px }
}

#[test]
fn container_morph_golden() {
    use vo_scene::container::{self, Shape};
    use vo_scene::font::FontRegistry;
    use vo_scene::taffy::style::{AlignItems, JustifyContent};
    use vo_scene::text::ShapedLayout;
    use vo_scene::TextStyle;
    let fonts = FontRegistry::new();
    fonts.add_file(std::path::Path::new(FONT)).unwrap();
    let content = || {
        Element::column("content")
            .size(260.0, 76.0)
            .gap(4.0)
            .child(
                Element::row("head")
                    .gap(8.0)
                    .align_items(AlignItems::CENTER)
                    .child(
                        Element::shape("dot", vo_scene::display::ShapeKind::Circle)
                            .size(10.0, 10.0)
                            .fill(Color::hex(0xef4444, 1.0)),
                    )
                    .child(Element::label("title", "Listening").text_style(TextStyle::new(0, 15.0, 19.0))),
            )
            .child(
                Element::label("body", "The quick brown fox jumps over the lazy dog.").color(Color::hex(0xc8cad0, 1.0)),
            )
    };
    let view = |shape: Shape| {
        Element::<()>::panel("screen")
            .size(340.0, 130.0)
            .fill(Color::hex(CELL, 1.0))
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .child(container::morph("c", shape, Color::hex(0x2a2e38, 1.0), content()))
    };
    let (dot, pill, card) =
        (Shape::Dot(18.0), Shape::Pill { w: 180.0, h: 40.0 }, Shape::Card { w: 300.0, h: 100.0, radius: 18.0 });
    let mut scene = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
    scene.set_viewport(0.0, Size::new(340.0, 130.0), 1.0);
    for (t, s) in [(0.0, dot), (0.3, pill), (1.0, card), (1.8, pill), (2.5, dot)] {
        scene.apply(view(s), t);
    }
    let gpu = support::gpu();
    let mut off = Offscreen::with_fonts(&gpu.device, &gpu.queue, fonts);
    let mut frames = |times: &[f64]| -> Vec<Vec<u8>> {
        times.iter().map(|&t| off.render_to_rgba(&scene.sample(t), [340, 130], 1.0)).collect()
    };
    let rows = vec![
        // Dot to pill, with the overshoot.
        frames(&[0.3, 0.35, 0.4, 0.47, 0.8]),
        // Pill to card: the content is uncovered from the middle.
        frames(&[1.0, 1.05, 1.1, 1.17, 1.5]),
        // Card to pill to dot.
        frames(&[1.85, 1.95, 2.55, 2.62, 3.0]),
    ];
    let img = grid("story-container", 340, 130, rows);
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn meters_golden() {
    use vo_scene::meters::{Countdown, Hairline};
    let accent = Color::hex(0x7dd3fc, 1.0);
    // Reports that go up, dip (a re-estimate) and go up again.
    let reports: Arc<[(f64, f32)]> =
        Arc::from([(0.0, 0.15), (0.4, 0.45), (0.5, 0.3), (0.9, 0.42), (1.2, 0.8), (1.6, 1.0)].as_slice());
    let line = Arc::new(Hairline { reports, color: accent, ..Hairline::default() });
    let thick = Arc::new(Hairline { width: 3.0, ..(*line).clone() });
    let ring = Arc::new(Countdown { start: 0.0, secs: 4.0, color: accent, ..Countdown::default() });
    let frame = |f: Frame| f;
    let lines = |h: Arc<Hairline>| -> Vec<Frame> {
        [0.05, 0.45, 0.55, 0.95, 1.3, 2.0]
            .iter()
            .map(|&t| {
                let h = h.clone();
                frame(Arc::new(move |r: Rect, p: &mut Painter| {
                    h.paint(t, Rect::new(r.x + 12.0, r.y, r.w - 24.0, r.h), p)
                }))
            })
            .collect()
    };
    let rings: Vec<Frame> = [0.0, 0.8, 1.6, 2.4, 3.2, 4.0]
        .iter()
        .map(|&t| {
            let ring = ring.clone();
            frame(Arc::new(move |r: Rect, p: &mut Painter| {
                ring.paint(t, Rect::new(r.x + 0.5 * r.w - 22.0, r.y + 0.5 * r.h - 22.0, 44.0, 44.0), p)
            }))
        })
        .collect();
    let img = sheet("story-meters", 150.0, 56.0, vec![lines(line), lines(thick), rings]);
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn reveal_golden() {
    use vo_scene::path::StrokeStyle;
    use vo_scene::reveal::FrameReveal;
    let accent = Color::hex(0x7dd3fc, 1.0);
    let reveal = Arc::new(FrameReveal {
        start: 0.0,
        border: StrokeStyle::solid(1.5, Color::hex(0xffffff, 0.7)),
        hairlines: vec![0.36],
        brackets: Some((10.0, 6.0, StrokeStyle::solid(2.0, accent))),
        ..FrameReveal::default()
    });
    let frames: Vec<Frame> = [0.0, 0.03, 0.08, 0.14, 0.2, 0.28, 0.36, 0.45, 0.7]
        .iter()
        .map(|&t| {
            let f = reveal.clone();
            Arc::new(move |r: Rect, p: &mut Painter| {
                let frame = Rect::new(r.x + 16.0, r.y + 14.0, r.w - 32.0, r.h - 28.0);
                f.paint(t, frame, p);
                // Content: a status dot and two lines of text, entering once the frame closed.
                let a = f.content_alpha(t);
                if a > 0.0 {
                    use vo_scene::display::{Paint, ShapeInstance};
                    use vo_scene::geom::RRect;
                    let dy = 4.0 * (1.0 - a);
                    let c = |c: Color| Paint::solid(c.fade(a));
                    let (x, y) = (frame.x + 14.0, frame.y + 12.0 + dy);
                    p.shape(ShapeInstance::circle(Point::new(x + 4.0, y + 4.0), 4.0, c(Color::hex(0xef4444, 1.0))));
                    p.shape(ShapeInstance::rrect(RRect::new(Rect::new(x + 14.0, y, 60.0, 8.0), 4.0), c(Color::WHITE)));
                    for (i, w) in [0.8f32, 0.55].iter().enumerate() {
                        let ly = frame.y + frame.h * 0.36 + 10.0 + i as f32 * 14.0 + dy;
                        let lw = (frame.w - 28.0) * w;
                        p.shape(ShapeInstance::rrect(
                            RRect::new(Rect::new(x, ly, lw, 7.0), 3.5),
                            c(Color::hex(0x9ca3af, 1.0)),
                        ));
                    }
                }
            }) as Frame
        })
        .collect();
    let rows = vec![frames[..5].to_vec(), frames[4..].to_vec()];
    let img = sheet("story-reveal", 200.0, 110.0, rows);
    support::check(&img, Tolerance::default()).unwrap();
}

#[test]
fn erase_golden() {
    use vo_scene::display::GlyphInstance;
    use vo_scene::erase::Erase;
    use vo_scene::font::FontRegistry;
    use vo_scene::text::{ShapedLayout, TextLayout};
    use vo_scene::TextStyle;
    let fonts = FontRegistry::new();
    fonts.add_file(std::path::Path::new(FONT)).unwrap();
    let layout = ShapedLayout::new(fonts.clone());
    let text = "send the draft to the team before noon";
    let style = TextStyle::new(0, 15.0, 20.0);
    let shaped = layout.layout(text, &style, Some(190.0));
    // Words from the glyph clusters: a new word starts after a space.
    let mut words: Vec<(Rect, Vec<usize>)> = Vec::new();
    let starts: Vec<usize> = std::iter::once(0).chain(text.match_indices(' ').map(|(i, _)| i + 1)).collect();
    for (gi, g) in shaped.glyphs.iter().enumerate() {
        let w = starts.iter().rposition(|&s| s as u32 <= g.cluster).unwrap();
        if text.as_bytes()[g.cluster as usize] == b' ' {
            continue;
        }
        while words.len() <= w {
            words.push((Rect::new(g.pos.x, g.pos.y - 15.0, 0.0, 20.0), Vec::new()));
        }
        let r = &mut words[w].0;
        r.w = g.pos.x + g.advance - r.x;
        words[w].1.push(gi);
    }
    let glyphs = Arc::new(shaped.glyphs);
    let words = Arc::new(words);
    let frames: Vec<Frame> = [0.0, 0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.42, 0.55]
        .iter()
        .map(|&t| {
            let (glyphs, words) = (glyphs.clone(), words.clone());
            Arc::new(move |r: Rect, p: &mut Painter| {
                let (ox, oy) = (r.x + 10.0, r.y + 22.0);
                let boxes = words.iter().map(|(b, _)| Rect::new(b.x + ox, b.y + oy, b.w, b.h)).collect();
                let e = Erase { start: 0.0, words: boxes, ..Erase::default() };
                for (i, (_, gs)) in words.iter().enumerate() {
                    let s = e.word(i, t);
                    for &gi in gs {
                        let g = glyphs[gi];
                        p.glyph(GlyphInstance {
                            pos: Point::new(g.pos.x + ox, g.pos.y + oy + s.dy),
                            size: 15.0,
                            font: 0,
                            glyph: g.glyph,
                            word: i as u32,
                            color: Color::hex(0xe5e7eb, s.opacity),
                            xform: vo_scene::display::GlyphXform::IDENTITY,
                        });
                    }
                }
                e.paint_strike(t, p);
            }) as Frame
        })
        .collect();
    let rows = vec![frames[..5].to_vec(), frames[5..].to_vec()];
    let img = sheet_with("story-erase", 210.0, 70.0, rows, Some(fonts));
    support::check(&img, Tolerance::default()).unwrap();
}
