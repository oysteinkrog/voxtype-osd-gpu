//! Vector animation spike: paths that draw themselves, shape and icon morphs, blob merge, a
//! light head on a panel border, text on a path and a size morph, 12 s at 1280x720 and 60 fps.
//!
//! The app is one canvas element (`Element::canvas`) whose draw function is a pure function of
//! the time: the timeline (`vo_core::timeline`) says when each part moves and its closed-form
//! springs say how. The same app exports to video and runs as a live overlay.
//!
//!   cargo run --release -p vo --example vector_demo -- [--out DIR] [--check] [--bench] [--live]
//!
//! Writes `DIR/vector_demo.mp4` (default DIR `/var/tmp/claude/m4-vector`) and
//! `DIR/contact-sheet.png`. `--check` renders twice and compares every frame's hash.
//! `--bench` times every frame on the GPU with timestamp queries and prints the cold cost of
//! the vector pipeline and of the first path fill. `--live` plays it in a loop as an overlay.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use vo::scene::canvas::Painter;
use vo::scene::display::{DisplayList, GlyphInstance, Paint, ShapeInstance, NO_WORD};
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Affine, Color, Point, RRect, Rect, Size};
use vo::scene::path::kurbo::{self, Shape};
use vo::scene::path::{fill_glyph, icons, Morph, Outline, StrokeStyle};
use vo::scene::text::TextLayout;
use vo::scene::Scene;
use vo::{App, Cx, Export, FontRegistry, Options, OverlaySpec, Placement, Scenario, ShapedLayout, Tokens, Video};
use vo_core::spring::Spring;
use vo_core::timeline::{all, clip, delay, stagger, Schedule, Timeline};
use vo_core::track::CubicBezier;
use vo_render::offscreen::{write_png, Frame};
use vo_render::{Renderer, Target};

const W: f32 = 1280.0;
const H: f32 = 720.0;
const FPS: f64 = 60.0;
const DURATION: f64 = 18.0;

fn bg() -> Color {
    Color::hex(0x0e1015, 1.0)
}
fn panel_fill() -> Color {
    Color::hex(0x1a1d24, 1.0)
}
fn ink() -> Color {
    Color::hex(0xe8eaf0, 1.0)
}
fn accent() -> Color {
    Color::hex(0x4ade80, 1.0)
}
fn warm() -> Color {
    Color::hex(0xf87171, 1.0)
}
fn cool() -> Color {
    Color::hex(0x7dd3fc, 1.0)
}

fn mix(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let l = |x: f32, y: f32| x + (y - x) * t;
    Color::rgba(l(a.r, b.r), l(a.g, b.g), l(a.b, b.b), l(a.a, b.a))
}

/// When each part moves.
fn timeline() -> Timeline {
    all([
        // Act 1: three icons draw themselves, then fill.
        delay(0.3, stagger(0.3, 0.6, ["draw.0", "draw.1", "draw.2"].map(|n| clip(n, 1.2)))),
        delay(2.0, clip("fill", 0.5)),
        delay(2.6, clip("act1.out", 0.4)),
        // Act 2: dot, capsule, panel; a light head runs the border; two dots merge.
        delay(3.0, clip("dot", 0.3)),
        delay(3.35, clip("capsule", 0.5)),
        delay(3.9, clip("panel", 0.8)),
        delay(4.3, clip("head", 2.0)),
        delay(4.4, clip("label", 0.4)),
        delay(3.3, clip("merge.in", 1.2)),
        delay(4.9, clip("merge.out", 1.0)),
        delay(6.0, clip("act2.out", 0.4)),
        // Act 3: mic to check to stop.
        delay(6.4, clip("icon.in", 0.4)),
        delay(7.0, clip("morph.1", 1.1)),
        delay(8.1, clip("morph.2", 0.9)),
        delay(9.0, clip("act3.out", 0.3)),
        // Act 4: a path draws on, text runs along it; a word shrinks.
        delay(9.3, clip("curve", 0.8)),
        delay(9.5, clip("ride", 2.2)),
        delay(9.6, clip("word.in", 0.4)),
        delay(10.6, clip("shrink", 0.8)),
        delay(11.6, clip("act4.out", 0.4)),
        // Act 5: spinner and progress, decorations under words, widgets, a glyph morph.
        delay(12.0, clip("act5.in", 0.4)),
        delay(13.0, clip("prog.1", 0.6)),
        delay(13.8, clip("prog.2", 0.6)),
        delay(14.6, clip("prog.3", 0.6)),
        delay(15.3, clip("done", 0.6)),
        delay(12.6, clip("spark", 1.6)),
        delay(13.2, clip("underline", 0.7)),
        delay(13.9, clip("wavy", 0.5)),
        delay(14.4, clip("chip", 0.5)),
        delay(14.0, clip("glyph.1", 1.0)),
        delay(15.8, clip("glyph.2", 1.0)),
        delay(17.6, clip("act5.out", 0.4)),
        clip("all", DURATION),
    ])
}

/// Everything built once: flattened icons, morphs, the registered fill paths, shaped text.
struct Assets {
    sched: Schedule,
    icons: [Outline; 3],
    fills: [u32; 3],
    morph1: Morph,
    morph2: Morph,
    curve: Outline,
    ride: Vec<(u32, f32, f32)>,
    ride_width: f32,
    word: Vec<(u32, f32)>,
    word_width: f32,
    font: u32,
    /// Act 5 sentence: glyphs as (id, pen x), and each word's x range, at `REF_SIZE`.
    sentence: Vec<(u32, f32)>,
    words: Vec<(f32, f32)>,
    glyph_morph: Morph,
}

const REF_SIZE: f32 = 100.0;

fn shape_line(layout: &ShapedLayout, font: u32, text: &str) -> (Vec<(u32, f32)>, f32) {
    let shaped = layout.layout(text, &TextStyle::new(font, REF_SIZE, REF_SIZE * 1.3), None);
    (shaped.glyphs.iter().map(|g| (g.glyph, g.pos.x)).collect(), shaped.size.w)
}

impl Assets {
    fn new(fonts: &FontRegistry) -> Assets {
        let font = fonts
            .add_system("Noto Sans")
            .or_else(|_| fonts.add_file(&vo::scene::font::test_font_path()))
            .expect("a font");
        let layout = ShapedLayout::new(fonts.clone());
        let set = [icons::mic(), icons::check(), icons::stop()];
        let fills = [0, 1, 2].map(|i| fonts.add_path(icons::outline(&set[i], 2.0)));
        let flat = |p: &kurbo::BezPath| Outline::new(p, 0.005);
        let icons = [flat(&set[0]), flat(&set[1]), flat(&set[2])];
        let morph1 = Morph::new(&icons[0], &icons[1], 128);
        let morph2 = Morph::new(&icons[1], &icons[2], 128);
        let mut curve = kurbo::BezPath::new();
        curve.move_to((140.0, 360.0));
        curve.curve_to((420.0, 120.0), (760.0, 600.0), (1140.0, 330.0));
        let curve = Outline::new(&curve, 0.05);
        let (glyphs, ride_width) = shape_line(&layout, font, "time is an input to every frame");
        // Each glyph's centre along the line, from the next glyph's pen position.
        let ride = glyphs
            .iter()
            .enumerate()
            .map(|(i, &(g, x))| {
                let next = glyphs.get(i + 1).map_or(ride_width, |n| n.1);
                (g, x, 0.5 * (next - x))
            })
            .collect();
        let (word, word_width) = shape_line(&layout, font, "Dictation");
        let text = "trim draws every word";
        let shaped = layout.layout(text, &TextStyle::new(font, REF_SIZE, REF_SIZE * 1.3), None);
        let sentence = shaped.glyphs.iter().map(|g| (g.glyph, g.pos.x)).collect();
        // Word x ranges from cluster offsets: a word runs from its first glyph to the pen
        // position of the glyph after its last one.
        let mut words = Vec::new();
        let mut start = 0usize;
        for w in text.split(' ') {
            let end = start + w.len();
            let xs: Vec<usize> = (0..shaped.glyphs.len())
                .filter(|&i| (start..end).contains(&(shaped.glyphs[i].cluster as usize)))
                .collect();
            let x0 = shaped.glyphs[xs[0]].pos.x;
            let after = xs.last().unwrap() + 1;
            let x1 = shaped.glyphs.get(after).map_or(shaped.size.w, |g| g.pos.x);
            // The pen position after a word includes its space; take the space's advance off.
            let x1 = if after < shaped.glyphs.len() { x1 - 0.26 * REF_SIZE } else { x1 };
            words.push((x0, x1));
            start = end + 1;
        }
        let face = fonts.get(font).expect("font");
        let gid = |c: &str| shape_line(&layout, font, c).0[0].0;
        let ga = Outline::new(&vo::scene::path::glyph_path(&face, gid("a"), 260.0), 0.02);
        let gg = Outline::new(&vo::scene::path::glyph_path(&face, gid("g"), 260.0), 0.02);
        let glyph_morph = Morph::new(&ga, &gg, 220);
        Assets {
            sched: timeline().schedule(),
            icons,
            fills,
            morph1,
            morph2,
            curve,
            ride,
            ride_width,
            word,
            word_width,
            font,
            sentence,
            words,
            glyph_morph,
        }
    }
}

/// The demo at `t` seconds into it, drawn in `rect` (the canvas box, 1280x720).
fn paint(a: &Assets, t: f64, rect: Rect, p: &mut Painter) {
    let s = &a.sched;
    p.layer(Affine::translate(rect.x, rect.y), 1.0, |b| {
        b.shape(ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, W, H), 0.0), Paint::solid(bg())));
        let out = |name: &str| 1.0 - s.eased(name, t, CubicBezier::IN) as f32;
        if t < 3.0 {
            act_icons(a, t, out("act1.out"), b);
        } else if t < 6.4 {
            act_panel(a, t, out("act2.out"), b);
        } else if t < 9.3 {
            act_morph(a, t, out("act3.out"), b);
        } else if t < 12.0 {
            act_text(a, t, out("act4.out"), b);
        } else {
            act_widgets(a, t, out("act5.out"), b);
        }
    });
}

/// A pen tip: the last `len` units of `o` up to `head`, brighter and glowing.
fn pen_tip(b: &mut Painter, o: &Outline, head: f32, len: f32, width: f32, color: Color) {
    let style = StrokeStyle::solid(width, color).ramp(head - len, color.fade(0.0), head, Color::WHITE).glow(width);
    b.outline(&o.trim(head - len, head), &style);
}

fn act_icons(a: &Assets, t: f64, fade: f32, b: &mut Painter) {
    let s = &a.sched;
    let k = 5.0;
    let draw = Spring::critical_to_98(1.0);
    let fill = s.eased("fill", t, CubicBezier::OUT) as f32;
    b.layer(Affine::IDENTITY, fade, |b| {
        for i in 0..3 {
            let at = Affine::scale(k).then(&Affine::translate(640.0 - 60.0 + (i as f32 - 1.0) * 220.0, 300.0));
            let o = a.icons[i].transform(&at);
            let f = s.spring(&format!("draw.{i}"), t, draw, 0.0, 1.0) as f32;
            let head = f * o.length();
            let color = [ink(), accent(), warm()][i];
            b.outline(&o.trim(0.0, head), &StrokeStyle::solid(10.0, color.fade(1.0 - fill)));
            if f < 0.995 && f > 0.0 {
                pen_tip(b, &o, head, 40.0, 10.0, color);
            }
            if fill > 0.0 {
                let pos = at.apply(Point::ZERO);
                b.glyph(fill_glyph(a.fills[i], pos, k, color.fade(fill)));
            }
        }
    });
}

fn act_panel(a: &Assets, t: f64, fade: f32, b: &mut Painter) {
    let s = &a.sched;
    let pop = |n: &str| s.spring(n, t, Spring::POP, 0.0, 1.0) as f32;
    let (dot, cap, pan) = (pop("dot"), pop("capsule"), pop("panel"));
    b.layer(Affine::IDENTITY, fade, |b| {
        // Dot to capsule to panel: size and radius on springs, one rounded rect.
        let w = (18.0 + 182.0 * cap + 400.0 * pan) * dot;
        let h = (18.0 + 102.0 * pan) * dot;
        let r = (0.5 * h) + (22.0 - 0.5 * h) * pan.clamp(0.0, 1.0);
        let (cx, cy) = (640.0, 290.0);
        let rect = RRect::new(Rect::new(cx - 0.5 * w, cy - 0.5 * h, w, h), r.max(0.0));
        let fill = mix(accent(), panel_fill(), cap);
        b.shape(
            ShapeInstance::rrect(rect, Paint::solid(fill))
                .with_hairline(Paint::solid(Color::hex(0x3a3f4b, pan.clamp(0.0, 1.0)))),
        );
        // The light head: a window on the border, wrapping the seam, fading tail and glow.
        if s.active("head", t) || s.progress("head", t) >= 1.0 {
            let border = Outline::new(
                &kurbo::RoundedRect::new(
                    rect.rect.x as f64,
                    rect.rect.y as f64,
                    rect.rect.right() as f64,
                    rect.rect.bottom() as f64,
                    rect.radii[0] as f64,
                )
                .to_path(0.01),
                0.05,
            );
            let total = border.length();
            let lt = s.local("head", t) as f32;
            let speed = 900.0;
            let fade_in = (lt / 0.3).clamp(0.0, 1.0);
            for (phase, color) in [(0.0, cool()), (0.5, accent())] {
                let head = lt * speed + phase * total;
                let tail = 260.0;
                let style = StrokeStyle::solid(2.5, color)
                    .ramp(head - tail, color.fade(0.0), head, mix(color, Color::WHITE, 0.6).fade(fade_in))
                    .glow(7.0);
                b.stroke(&border.window(0, head, tail), &style);
            }
        }
        // Label in the panel.
        let lab = s.eased("label", t, CubicBezier::OUT) as f32;
        if lab > 0.0 {
            let size = 34.0;
            let k = size / REF_SIZE;
            let x0 = cx - 0.5 * a.word_width * k;
            for &(g, x) in &a.word {
                b.glyph(GlyphInstance {
                    pos: Point::new(x0 + x * k, cy + 12.0 + 8.0 * (1.0 - lab)),
                    size,
                    font: a.font,
                    glyph: g,
                    word: NO_WORD,
                    color: ink().fade(lab),
                });
            }
        }
        // Two dots that merge and split again (smooth-min union).
        let d = 140.0 * (1.0 - pop("merge.in")) + 140.0 * pop("merge.out");
        let (ya, ra, rb) = (540.0, 26.0, 18.0);
        let grow = s.spring("merge.in", t + 0.3, Spring::POP, 0.0, 1.0) as f32;
        if grow > 0.0 {
            b.blob(
                (Point::new(640.0 - d, ya), ra * grow),
                (Point::new(640.0 + d, ya), rb * grow),
                34.0,
                cool(),
                Color::hex(0xc084fc, 1.0),
            );
        }
    });
}

fn act_morph(a: &Assets, t: f64, fade: f32, b: &mut Painter) {
    let s = &a.sched;
    let inn = s.spring("icon.in", t, Spring::POP, 0.0, 1.0) as f32;
    let m1 = s.spring("morph.1", t, Spring::from_frequency(11.0, 0.62), 0.0, 1.0) as f32;
    let m2 = s.spring("morph.2", t, Spring::from_frequency(11.0, 0.62), 0.0, 1.0) as f32;
    let k = 10.0 * (0.6 + 0.4 * inn);
    let at = Affine::translate(-12.0, -12.0).then(&Affine::scale(k)).then(&Affine::translate(640.0, 330.0));
    let (parts, color) = if s.local("morph.2", t) <= 0.0 {
        (a.morph1.sample(m1), mix(ink(), accent(), m1))
    } else {
        (a.morph2.sample(m2), mix(accent(), warm(), m2))
    };
    b.layer(Affine::IDENTITY, fade * inn.clamp(0.0, 1.0), |b| {
        for p in parts {
            let style = StrokeStyle::solid(2.2 * k * p.weight, color.fade(p.weight.sqrt())).glow(3.0 * p.weight);
            b.stroke(&p.contour.transform(&at), &style);
        }
    });
}

fn act_text(a: &Assets, t: f64, fade: f32, b: &mut Painter) {
    let s = &a.sched;
    b.layer(Affine::IDENTITY, fade, |b| {
        // The curve draws on.
        let total = a.curve.length();
        let f = s.spring("curve", t, Spring::critical_to_98(0.8), 0.0, 1.0) as f32;
        b.outline(&a.curve.trim(0.0, f * total), &StrokeStyle::solid(1.5, Color::hex(0x3a3f4b, 1.0)));
        // Text rides along it: each glyph on its own layer, rotated to the tangent.
        let size = 40.0;
        let k = size / REF_SIZE;
        let ride = s.eased("ride", t, CubicBezier::new(0.3, 0.0, 0.2, 1.0)) as f32;
        let start = -a.ride_width * k + ride * (0.5 * (total + a.ride_width * k));
        for &(g, x, half) in &a.ride {
            let along = start + (x + half) * k;
            if along < 0.0 || along > f * total {
                continue;
            }
            let Some((p, tan)) = a.curve.contours[0].at(along) else { continue };
            let angle = tan.y.atan2(tan.x);
            let edge = (along / 60.0).min((f * total - along) / 60.0).clamp(0.0, 1.0);
            b.layer(Affine::rotate(angle).then(&Affine::translate(p.x, p.y)), edge, |b| {
                b.glyph(GlyphInstance {
                    pos: Point::new(-half * k, -10.0),
                    size,
                    font: a.font,
                    glyph: g,
                    word: NO_WORD,
                    color: mix(cool(), ink(), (x / a.ride_width).clamp(0.0, 1.0)),
                });
            });
        }
        // Size morph: the word scales from 72 to 44 on a spring, instances only.
        let inn = s.eased("word.in", t, CubicBezier::OUT) as f32;
        let size = s.spring("shrink", t, Spring::POP, 72.0, 44.0) as f32;
        let k = size / REF_SIZE;
        let x0 = 640.0 - 0.5 * a.word_width * k;
        for &(g, x) in &a.word {
            b.glyph(GlyphInstance {
                pos: Point::new(x0 + x * k, 610.0),
                size,
                font: a.font,
                glyph: g,
                word: NO_WORD,
                color: ink().fade(inn),
            });
        }
    });
}

/// The demo as an app: one canvas element the size of the surface.
fn act_widgets(a: &Assets, t: f64, fade: f32, b: &mut Painter) {
    use vo::scene::path::make;
    let s = &a.sched;
    let inn = s.eased("act5.in", t, CubicBezier::OUT) as f32;
    let soft = |n: &str| s.spring(n, t, Spring::SOFT, 0.0, 1.0) as f32;
    let track = Color::hex(0x262a33, 1.0);
    b.layer(Affine::IDENTITY, fade * inn, |b| {
        // Spinner, then a determinate arc that springs to each value and never goes back,
        // then the ring closes and a check draws in it.
        let c = Point::new(200.0, 200.0);
        b.stroke(&make::ring(c, 56.0), &StrokeStyle::solid(10.0, track));
        let lt = s.local("act5.in", t);
        let determinate = s.local("prog.1", t) > 0.0;
        if !determinate {
            b.stroke(&make::spinner(c, 56.0, lt.max(0.0)), &StrokeStyle::solid(10.0, cool()));
        } else {
            let v = 0.3 * soft("prog.1") + 0.35 * soft("prog.2") + 0.35 * soft("prog.3");
            let done = soft("done");
            let color = mix(cool(), accent(), done);
            b.stroke(&make::progress(c, 56.0, v), &StrokeStyle::solid(10.0, color).glow(4.0 * done));
            if done > 0.0 {
                let check = a.icons[1].transform(
                    &Affine::translate(-12.0, -12.0).then(&Affine::scale(3.2)).then(&Affine::translate(c.x, c.y)),
                );
                let len = check.length();
                b.outline(&check.trim(0.0, done * len), &StrokeStyle::solid(9.0, accent()));
            }
        }
        // A sentence with decorations tied to its words' measured bounds.
        let size = 44.0;
        let k = size / REF_SIZE;
        let (x0, base) = (380.0, 210.0);
        for &(g, x) in &a.sentence {
            b.glyph(GlyphInstance {
                pos: Point::new(x0 + x * k, base),
                size,
                font: a.font,
                glyph: g,
                word: NO_WORD,
                color: ink(),
            });
        }
        let word = |i: usize| (x0 + a.words[i].0 * k, x0 + a.words[i].1 * k);
        // Highlight chip behind "every": grows from the left on a spring.
        let chip = s.spring("chip", t, Spring::POP, 0.0, 1.0) as f32;
        if chip > 0.0 {
            let (w0, w1) = word(2);
            let r = RRect::new(Rect::new(w0 - 6.0, base - 36.0, (w1 - w0 + 12.0) * chip, 48.0), 10.0);
            b.shape(ShapeInstance::rrect(r, Paint::solid(Color::hex(0x7c3aed, 0.35))));
        }
        // Draw-on underline under "trim", from the left, with a gradient along it.
        let (u0, u1) = word(0);
        let under = make::underline(u0, u1, base + 10.0);
        let f = soft("underline");
        b.stroke(
            &under.trim(0.0, f * (u1 - u0)),
            &StrokeStyle::solid(4.0, accent()).ramp(0.0, cool(), u1 - u0, accent()),
        );
        // Wavy underline under "draws": draws on, then keeps flowing.
        let (v0, v1) = word(1);
        let flow = (s.local("wavy", t).max(0.0) * 6.0) as f32;
        let wavy = make::wave(v0, v1, base + 12.0, 3.0, 14.0, -flow);
        let wl = wavy.length();
        b.stroke(&wavy.trim(0.0, soft("wavy") * wl), &StrokeStyle::solid(2.5, warm()));
        // A sparkline that draws on, a dashed frame that marches, a conic rim that turns.
        let values = [0.3, 0.45, 0.4, 0.7, 0.55, 0.8, 0.6, 0.9, 0.75, 0.95, 0.7, 1.0];
        let spark = make::sparkline(&values, 980.0, 140.0, 200.0, 80.0);
        let sl = spark.length();
        b.stroke(&spark.trim(0.0, soft("spark") * sl), &StrokeStyle::solid(3.0, accent()).glow(3.0));
        let march = (lt.max(0.0) * 30.0) as f32;
        for d in make::panel(960.0, 120.0, 240.0, 120.0, 16.0).dash(&[10.0, 7.0], march) {
            b.stroke(&d, &StrokeStyle::solid(2.0, Color::hex(0x6b7280, 1.0)));
        }
        let rc = Point::new(1080.0, 440.0);
        let spin = (lt * 1.6) as f32;
        b.stroke(
            &make::ring(rc, 70.0),
            &StrokeStyle::solid(6.0, Color::WHITE)
                .conic(rc, spin, Color::hex(0x22d3ee, 1.0), Color::hex(0xa855f7, 1.0))
                .glow(5.0),
        );
        // A ribbon from two travelling sines, its width swelling along it.
        let n = 90;
        let pts: Vec<Point> = (0..=n)
            .map(|i| {
                let x = 60.0 + 760.0 * i as f32 / n as f32;
                let ph = lt as f32 * 2.2;
                Point::new(x, 560.0 + 30.0 * (x * 0.012 + ph).sin() + 14.0 * (x * 0.031 - ph * 1.7).sin())
            })
            .collect();
        let widths: Vec<f32> = (0..=n)
            .map(|i| {
                0.15 + (std::f32::consts::PI * i as f32 / n as f32).sin()
                    * (0.8 + 0.2 * (i as f32 * 0.3 + lt as f32 * 3.0).sin())
            })
            .collect();
        b.stroke(
            &make::ribbon(&pts, &widths),
            &StrokeStyle::solid(18.0, Color::WHITE)
                .linear(
                    Point::new(60.0, 0.0),
                    Color::hex(0x60a5fa, 1.0),
                    Point::new(820.0, 0.0),
                    Color::hex(0xf472b6, 1.0),
                )
                .glow(6.0),
        );
        // A glyph morph as a fill with holes: a to g and back.
        let m = s.spring("glyph.1", t, Spring::from_frequency(12.0, 0.65), 0.0, 1.0)
            - s.spring("glyph.2", t, Spring::from_frequency(12.0, 0.65), 0.0, 1.0);
        let at = Affine::translate(560.0, 480.0);
        let parts: Vec<_> = a
            .glyph_morph
            .sample(m as f32)
            .into_iter()
            .filter(|p| p.weight > 0.01)
            .map(|p| p.contour.transform(&at))
            .collect();
        b.fill(
            &parts,
            &StrokeStyle::solid(0.0, Color::WHITE).linear(
                Point::new(560.0, 300.0),
                cool(),
                Point::new(560.0, 560.0),
                ink(),
            ),
        );
    });
}

struct Demo {
    assets: Arc<Assets>,
    /// Scene time the demo starts at (the host clock's time at `init` when live).
    start: f64,
    /// Live: play again and again.
    looped: bool,
}

#[derive(Clone, Debug)]
enum Msg {}

impl App for Demo {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        self.start = cx.t();
    }

    fn update(&mut self, msg: Msg, _cx: &mut Cx<Msg>) {
        match msg {}
    }

    fn view(&self, _tokens: &Tokens) -> Element<Msg> {
        let (a, start, looped) = (self.assets.clone(), self.start, self.looped);
        let until = if looped { f64::INFINITY } else { start + DURATION };
        Element::canvas("demo", until, move |t, rect, p| {
            let lt = if looped { (t - start).rem_euclid(DURATION) } else { (t - start).clamp(0.0, DURATION) };
            paint(&a, lt, rect, p)
        })
        .size(W, H)
    }
}

struct Args {
    out: PathBuf,
    check: bool,
    bench: bool,
    live: bool,
}

fn args() -> Args {
    let mut a = Args { out: PathBuf::from("/var/tmp/claude/m4-vector"), check: false, bench: false, live: false };
    let mut it = std::env::args().skip(1);
    while let Some(x) = it.next() {
        match x.as_str() {
            "--out" => a.out = it.next().expect("--out DIR").into(),
            "--check" => a.check = true,
            "--bench" => a.bench = true,
            "--live" => a.live = true,
            other => panic!("unknown argument {other}"),
        }
    }
    a
}

fn device(timestamps: bool) -> (wgpu::Device, wgpu::Queue) {
    let mut idesc = wgpu::InstanceDescriptor::new_without_display_handle();
    idesc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(idesc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("a Vulkan adapter");
    let features = if timestamps {
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS
    } else {
        wgpu::Features::empty()
    };
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("vector-demo"),
        required_features: features,
        ..Default::default()
    }))
    .expect("a GPU device")
}

fn frame_count() -> u64 {
    (DURATION * FPS).round() as u64
}

fn options(fonts: &FontRegistry) -> Options<Msg> {
    Options { fonts: fonts.clone(), ..Options::default() }
}

fn scenario() -> Scenario<Msg> {
    Scenario { size: Size::new(W, H), scale: 1.0, duration: DURATION, inputs: Vec::new() }
}

/// A 4 x 4 grid of frames at half size, composited over black.
fn write_sheet(path: &std::path::Path, frames: &[Frame]) {
    let (fw, fh) = (W as usize / 2, H as usize / 2);
    let (cols, rows) = (4, frames.len().div_ceil(4));
    let (sw, sh) = (cols * fw + (cols + 1) * 8, rows * fh + (rows + 1) * 8);
    let mut px = vec![0u8; sw * sh * 4];
    for p in px.as_chunks_mut::<4>().0 {
        *p = [40, 40, 44, 255];
    }
    for (i, f) in frames.iter().enumerate() {
        let (ox, oy) = (8 + (i % cols) * (fw + 8), 8 + (i / cols) * (fh + 8));
        for y in 0..fh {
            for x in 0..fw {
                let mut acc = [0u32; 3];
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let s = ((2 * y + dy) * f.width as usize + 2 * x + dx) * 4;
                    for (a, v) in acc.iter_mut().zip(&f.rgba[s..s + 3]) {
                        *a += *v as u32;
                    }
                }
                let d = ((oy + y) * sw + ox + x) * 4;
                px[d..d + 4].copy_from_slice(&[(acc[0] / 4) as u8, (acc[1] / 4) as u8, (acc[2] / 4) as u8, 255]);
            }
        }
    }
    write_png(path, sw as u32, sh as u32, &px).expect("write contact sheet");
}

/// GPU time per frame from timestamp queries, and the cold cost of the new pipelines.
fn bench(assets: Arc<Assets>, fonts: &FontRegistry) {
    let (device, queue) = device(true);
    let mut r = Renderer::with_fonts(&device, &queue, fonts.clone());
    let mut target = Target::new(&device, W as u32, H as u32, vo_render::offscreen::FORMAT);
    let wait = |d: &wgpu::Device| d.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
    let demo = Demo { assets, start: 0.0, looped: false };
    let mut scene: Scene<Msg> = Scene::new();
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(demo.view(&Tokens::default()), 0.0);
    let frame = |t: f64| scene.sample(t);

    // Cold costs: shapes first (warms the shape pipeline), then the first frame with vectors,
    // then the first path fill, each timed on the CPU including the GPU wait.
    let mut base = DisplayList { size: Size::new(W, H), scale: 1.0, ..Default::default() };
    base.shapes.push(ShapeInstance::rrect(RRect::new(Rect::new(0.0, 0.0, W, H), 0.0), Paint::solid(bg())));
    base.items.push(vo::scene::display::Item { kind: vo::scene::display::ItemKind::Shapes, start: 0, len: 1 });
    base.layers.push(vo::scene::display::Layer { items: 0..1, ..vo::scene::display::Layer::root() });
    r.render(&base, &mut target);
    wait(&device);
    let timed = |r: &mut Renderer, list: &DisplayList, target: &mut Target| {
        let t0 = Instant::now();
        r.render(list, target);
        wait(&device);
        t0.elapsed().as_secs_f64() * 1e3
    };
    let vec_frame = frame(8.5);
    let cold_vec = timed(&mut r, &vec_frame, &mut target);
    let warm_vec = timed(&mut r, &vec_frame, &mut target);
    let fill_frame = frame(2.4);
    let cold_fill = timed(&mut r, &fill_frame, &mut target);
    let warm_fill = timed(&mut r, &fill_frame, &mut target);
    println!("cold first frame with vectors: {cold_vec:.2} ms (warm {warm_vec:.2} ms)");
    println!("cold first frame with three path fills (path encode): {cold_fill:.2} ms (warm {warm_fill:.2} ms)");

    let qs =
        device.create_query_set(&wgpu::QuerySetDescriptor { label: None, ty: wgpu::QueryType::Timestamp, count: 2 });
    let resolve = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let read = device.create_buffer(&wgpu::BufferDescriptor {
        label: None,
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let period = queue.get_timestamp_period() as f64;
    let mut per_act: [Vec<f64>; 5] = Default::default();
    let mut cpu_build = Vec::new();
    for i in 0..frame_count() {
        let t = i as f64 / FPS;
        let c0 = Instant::now();
        let list = frame(t);
        cpu_build.push(c0.elapsed().as_secs_f64() * 1e3);
        let mut enc = device.create_command_encoder(&Default::default());
        enc.write_timestamp(&qs, 0);
        r.encode(&list, 1.0, &target, &mut enc);
        enc.write_timestamp(&qs, 1);
        enc.resolve_query_set(&qs, 0..2, &resolve, 0);
        enc.copy_buffer_to_buffer(&resolve, 0, &read, 0, 16);
        queue.submit([enc.finish()]);
        read.slice(..).map_async(wgpu::MapMode::Read, |_| {});
        wait(&device);
        let ts: [u64; 2] = read_u64x2(&read.slice(..).get_mapped_range().expect("mapped"));
        read.unmap();
        let ms = (ts[1].wrapping_sub(ts[0])) as f64 * period / 1e6;
        let act = if t < 3.0 {
            0
        } else if t < 6.4 {
            1
        } else if t < 9.3 {
            2
        } else if t < 12.0 {
            3
        } else {
            4
        };
        per_act[act].push(ms);
    }
    let stats = |v: &mut Vec<f64>| {
        v.sort_by(f64::total_cmp);
        let mean = v.iter().sum::<f64>() / v.len() as f64;
        (mean, v[v.len() / 2], v[(v.len() * 95) / 100], v[v.len() - 1])
    };
    for (name, v) in ["icons draw-on", "panel, head, blobs", "icon morph", "text on path", "widgets, fills"]
        .iter()
        .zip(per_act.iter_mut())
    {
        let (mean, med, p95, max) = stats(v);
        println!("GPU per frame, {name}: mean {mean:.3} ms, median {med:.3}, p95 {p95:.3}, max {max:.3} (1280x720)");
    }
    let (mean, _, p95, max) = stats(&mut cpu_build);
    println!("CPU scene.sample(t) (canvas: trim, morph, display list): mean {mean:.3} ms, p95 {p95:.3}, max {max:.3}");
}

fn read_u64x2(b: &[u8]) -> [u64; 2] {
    [u64::from_le_bytes(b[0..8].try_into().unwrap()), u64::from_le_bytes(b[8..16].try_into().unwrap())]
}

fn main() {
    let args = args();
    let fonts = FontRegistry::new();
    let assets = Arc::new(Assets::new(&fonts));
    if args.bench {
        bench(assets, &fonts);
        return;
    }
    if args.live {
        let spec = OverlaySpec {
            size: Size::new(W, H),
            placement: Placement::Centered,
            app_id: "vo-vector-demo".into(),
            ..OverlaySpec::default()
        };
        if let Err(e) = vo::run_overlay_with(Demo { assets, start: 0.0, looped: true }, spec, options(&fonts)) {
            eprintln!("vector_demo: {e}");
            std::process::exit(1);
        }
        return;
    }
    let mp4 = args.out.join("vector_demo.mp4");
    let spec = Export { fps: FPS, duration: DURATION, png_dir: None, video: Some(Video::h264(&mp4)) };
    let n = frame_count();
    let picks: Vec<u64> = (0..16).map(|i| ((i as f64 + 0.5) / 16.0 * n as f64) as u64).collect();
    let mut sheet = Vec::new();
    let t0 = Instant::now();
    let demo = Demo { assets: assets.clone(), start: 0.0, looped: false };
    let report = vo::export_with(demo, &scenario(), &spec, options(&fonts), |i, f| {
        if picks.contains(&i) {
            sheet.push(f.clone());
        }
    })
    .expect("export");
    let secs = t0.elapsed().as_secs_f64();
    write_sheet(&args.out.join("contact-sheet.png"), &sheet);
    println!("{} frames in {secs:.2} s -> {}", report.frames, mp4.display());
    println!("contact sheet: {}", args.out.join("contact-sheet.png").display());
    if args.check {
        let again = Export { video: None, ..spec };
        let demo = Demo { assets, start: 0.0, looped: false };
        let second = vo::export(demo, &scenario(), &again, options(&fonts)).expect("export");
        let same = report.frame_hashes == second.frame_hashes;
        println!("determinism: second render {}", if same { "matches every frame" } else { "DIFFERS" });
        assert!(same);
    }
}
