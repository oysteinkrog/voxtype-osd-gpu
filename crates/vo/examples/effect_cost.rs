//! GPU cost of each effect, from timestamps per pass (`vo_render::profile`).
//!
//! Every case draws the same popup-like card (a panel, a title and two lines of text) with
//! one effect on it, at 600x300 and at 1920x1080. A case's cost is its median GPU time per
//! frame; `+ms` is that minus the plain card's, so it is what the effect adds. The table also
//! gives the slowest section (the scene, a layer, a bloom pyramid) and the pooled texture
//! memory.
//!
//!   cargo run --release -p vo --example effect_cost -- [--frames N] [--out TABLE.md]
//!       [--max-ms MS]     exit 1 when a case costs more than MS at 600x300
//!       [--case NAME]...  only these cases (and the plain card, the base); repeatable
//!   cargo run --release -p vo --example effect_cost -- --preview   every case, 2 s each
//!   cargo run --release -p vo --example effect_cost -- --sheet OUT.png --frames 11
//!
//! `just budget` runs it and writes the table next to the budget log.

use std::time::Instant;

use vo::scene::depth_stack::{DepthStack, Place, Sheet};
use vo::scene::displace::{Displace, Onset, Ripples};
use vo::scene::display::{BackdropRes, TrailDrift};
use vo::scene::element::{Element, TextStyle};
use vo::scene::fluid::{FluidLayer, InkStyle, InkWord};
use vo::scene::geom::{Color, Point, Rect, Size};
use vo::scene::glass::{Glass, GlassKind, GlassShape};
use vo::scene::material::Material;
use vo::scene::path::{Contour, StrokeStyle, WidthKnot};
use vo::scene::text::TextLayout;
use vo::scene::water::{PoolDrop, WaterPool};
use vo::scene::Scene;
use vo::{App, Cx, FontRegistry, Options, Scenario, ShapedLayout};
use vo_render::profile::timestamp_features;
use vo_render::{Renderer, Target};
use vo_sim::water::{Water, WaterConfig, WaterDrop};

/// Seconds each case shows in the preview.
const CASE_SECS: f64 = 2.0;

/// The effects. Each is applied where it works: shape effects on the card's panel, text
/// effects on its labels, layer effects on a layer around the card.
const CASES: &[&str] = &[
    "plain",
    "glow",
    "shadow",
    "text halo",
    "text blur",
    "offscreen",
    "bloom",
    "bloom wide",
    "trail",
    "trail-drift",
    "motion blur",
    "crt style",
    "glitch",
    "backdrop",
    "backdrop half",
    "material backdrop",
    "glass",
    "glass octagon",
    "glass wavy",
    "glass lens",
    "glass prism",
    "layer blur r0",
    "layer blur r8",
    "frost r8",
    "frost stack",
    "displace ripples",
    "displace noise",
    "water pool",
    "ink",
    "smoke",
    "watercolour",
    "tapered strokes",
    "shape-group-8",
    "grid-words",
    "glyphs-2000-rotating",
    "panes-20",
];

/// The app material of the "material backdrop" case (EG-02): the card's panel reads its
/// layer's backdrop through four taps 3 px apart, a light frost, under its own fill.
const FROST: u32 = vo::scene::material::CUSTOM_FIRST;
const FROST_WGSL: &str = "
fn mat_frost (m: Material, i: MatIn) -> vec4f {
  var b = vec4f (0.0);
  for (var k = 0; k < 4; k++) {
    let a = f32 (k) * 1.5707963 + 0.785;
    b += tex_backdrop (i.px + 3.0 * vec2f (cos (a), sin (a)));
  }
  let under = 0.25 * b * 0.6;
  return (i.color + under * (1.0 - i.color.a)) * i.cov;
}
";

/// Breathing Grid (EG-31) for the "grid-words" case: the card's panel reads the text field
/// and the word table, and a new word comes every half second, so the field is rebuilt then.
const GRID: u32 = vo::scene::material::CUSTOM_FIRST + 1;
const GRID_WGSL: &str = include_str!("grid_words.wgsl");

fn add_materials(r: &mut Renderer) {
    r.add_material(FROST, "mat_frost", FROST_WGSL);
    r.add_material(GRID, "mat_grid_words", GRID_WGSL);
}

/// The live words of the "grid-words" case at `t`: one more every half second, round and
/// round.
fn grid_words(t: f64) -> vo_core::words::WordModel {
    const SENTENCE: &str = "so the window shows the live words";
    let all: Vec<&str> = SENTENCE.split(' ').collect();
    let n = (t / 0.5) as usize % all.len() + 1;
    let mut model = vo_core::words::WordModel::new();
    model.update(&all[..n].join(" "));
    model
}

#[derive(Clone, Debug, PartialEq)]
enum Msg {
    Case(usize),
}

/// The card with case `i`'s effect, at time `t`. `k` scales lengths with the surface, and the
/// card drifts sideways so trails and motion blur have something to show.
fn view(size: Size, i: usize, t: f64) -> Element<Msg> {
    let k = size.h / 300.0;
    let case = CASES[i];
    // The grid-words card holds still, so its text field is rebuilt only when a word comes.
    let drift = if case == "grid-words" { 0.0 } else { (t * 1.7).sin() as f32 * 24.0 * k };
    let text = |key: &str, s: &str, px: f32, c: u32| {
        let e = Element::label(key.to_string(), s)
            .text_style(TextStyle::new(0, px * k, px * 1.25 * k))
            .color(Color::hex(c, 1.0));
        match case {
            "text halo" => e.text_halo(Color::hex(0x4cc9f0, 0.8), 10.0 * k),
            "text blur" => e.text_blur(3.0 * k),
            _ => e,
        }
    };
    let mut card = Element::panel("card")
        .pad(18.0 * k)
        .gap(8.0 * k)
        .radius(14.0 * k)
        .fill(Color::hex(0x10141c, 0.92))
        .border(Color::hex(0x4cc9f0, 0.8), 1.5 * k)
        .child(text("title", case, 30.0, 0xffffff))
        .child(text("l1", "open the configuration file and set the theme", 17.0, 0xc8d3e0))
        .child(if case == "grid-words" {
            Element::live_text("l2", grid_words(t).words())
                .text_style(TextStyle::new(0, 17.0 * k, 17.0 * 1.25 * k))
                .color(Color::hex(0xc8d3e0, 1.0))
        } else {
            text("l2", "so the window shows the live words", 17.0, 0xc8d3e0)
        })
        .child(
            Element::wavy_line("wave", StrokeStyle::solid(2.0 * k, Color::hex(0x4cc9f0, 1.0)), 6.0 * k, 40.0 * k)
                .size(380.0 * k, 20.0 * k),
        )
        .offset(drift, 0.0);
    match case {
        "glow" => card = card.glow(Color::hex(0x4cc9f0, 0.9), 14.0 * k, 1.2),
        "shadow" => card = card.shadow(Color::rgba(0.0, 0.0, 0.0, 0.6), 0.0, 8.0 * k, 18.0 * k),
        "motion blur" => card = card.motion_blur(1.0 / 30.0),
        // The grid is drawn behind the card (below); the card lets it show through.
        "grid-words" => card = card.fill(Color::hex(0x10141c, 0.35)),
        "material backdrop" => {
            card = card.fill(Color::hex(0x10141c, 0.55)).material(Material::custom(FROST).reads_backdrop())
        }
        // Glass on the rounded rect, and on a chamfered octagon (EG-06), with the orbit on.
        "glass" => card = card.fill(Color::TRANSPARENT).glass(Glass::dark().with_orbit(Color::hex(0x4cc9f0, 1.0))),
        "glass octagon" => {
            let g = Glass::dark().with_orbit(Color::hex(0x4cc9f0, 1.0)).with_shape(GlassShape::chamfer(12.0 * k));
            card = card.fill(Color::TRANSPARENT).glass(g)
        }
        // Glass with wavy edges (EG-38): up to 512 outline points.
        "glass wavy" => {
            let shape = GlassShape::Wavy { amplitude: 3.0 * k, wavelength: 34.0 * k };
            let g = Glass::dark().with_orbit(Color::hex(0x4cc9f0, 1.0)).with_shape(shape);
            card = card.fill(Color::TRANSPARENT).glass(g)
        }
        // Refracting glass (EG-05) over the stripes: a lens disc and a prism.
        "glass lens" => card = card.fill(Color::TRANSPARENT).glass(Glass::lens(GlassKind::Dark)),
        "glass prism" => card = card.fill(Color::TRANSPARENT).glass(Glass::prism(GlassKind::Dark)),
        _ => {}
    }
    if case == "frost stack" {
        return frost_stack(size, k);
    }
    if case == "shape-group-8" {
        // EG-22: an 8-member shape group the card's width, drawn four times (shadow, glow,
        // fill with a stroke), the capsule's edge rippling and seven bulges moving.
        let blob = Element::canvas("group", f64::INFINITY, move |t, rect, p| shape_group_8(t, rect, k, p))
            .size(size.w, size.h)
            .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute);
        let root = Element::column("root").size(size.w, size.h).pad_xy(60.0 * k, 40.0 * k);
        return root.child(blob).child(Element::layer("fx").child(card));
    }
    if case == "glyphs-2000-rotating" {
        // EG-15: 2000 letters at 14 px over the card, each turning on its own, in one draw.
        let letters = Element::canvas("letters", f64::INFINITY, move |t, rect, p| rotating_glyphs(t, rect, k, p))
            .size(size.w, size.h)
            .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute);
        let root = Element::column("root").size(size.w, size.h).pad_xy(60.0 * k, 40.0 * k);
        return root.child(Element::layer("fx").child(card)).child(letters);
    }
    if case == "panes-20" {
        return panes_20(size, t);
    }
    if case == "tapered strokes" {
        // SW-04: 200 letter-sized strokes over the card, each tapering from 1 to 4 px.
        let strokes = Element::canvas("strokes", f64::INFINITY, move |t, rect, p| tapered_strokes(t, rect, k, p))
            .size(size.w, size.h)
            .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute);
        let root = Element::column("root").size(size.w, size.h).pad_xy(60.0 * k, 40.0 * k);
        return root.child(Element::layer("fx").child(card)).child(strokes);
    }
    let layer = Element::layer("fx").child(card);
    let layer = match case {
        "offscreen" => layer.offscreen(),
        "bloom" => layer.bloom(1.0, 18.0 * k),
        "bloom wide" => layer.bloom(1.0, 60.0 * k),
        "trail" => layer.trail(0.25),
        // EG-34: the same trail with its afterglow turning and growing about the card's middle.
        "trail-drift" => layer.trail(0.25).trail_drift(TrailDrift {
            turn_deg_per_s: 90.0,
            scale_per_s: 0.1,
            offset_px_per_s: [0.0; 2],
            pivot: Point::new(240.0 * k, 110.0 * k),
        }),
        "crt style" => layer.chromatic(2.0 * k).scanlines(0.35, 3.0 * k).crt(0.12).vignette(0.4),
        "glitch" => layer.glitch(0.6),
        // A blur of the card (EG-04); radius 0 must add no pass at all.
        "layer blur r0" => layer.layer_blur(0.0),
        "layer blur r8" => layer.layer_blur(8.0 * k),
        // The stripes under the card frosted inside its box (EG-04).
        "frost r8" => layer.frost(8.0 * k).radius(14.0 * k),
        // A layer the card's size that captures what is under it and draws it back tinted
        // (EG-01), the base cost of glass, blur and ripples over a backdrop.
        "backdrop" => layer.backdrop(BackdropRes::Full, Color::hex(0xff6b6b, 1.0)),
        "backdrop half" => layer.backdrop(BackdropRes::Half, Color::hex(0xff6b6b, 1.0)),
        // A backdrop that only captures, read by the panel's material (EG-02).
        "material backdrop" => layer.backdrop(BackdropRes::Full, Color::TRANSPARENT),
        // The glass reads the stripes under it through a backdrop that only captures (EG-05).
        "glass lens" | "glass prism" => layer.backdrop(BackdropRes::Full, Color::TRANSPARENT),
        // The stripes under the card drawn back displaced (EG-03): four rings alive at any
        // time, one every 0.3 s, and a turning noise field.
        "displace ripples" => {
            let last = (t / 0.3).floor() * 0.3;
            let onsets = (0..4)
                .map(|n| Onset { t: last - n as f64 * 0.3, at: Point::new((80.0 + 90.0 * n as f32) * k, 60.0 * k) });
            layer.displace(Displace::Ripples(Ripples::new().onsets(onsets).amp(5.0 * k).speed(160.0 * k)))
        }
        "displace noise" => layer.displace(Displace::Noise { scale: 50.0 * k, amount: 4.0 * k, speed: 0.25, seed: 3 }),
        _ => layer,
    };
    let root = Element::column("root").size(size.w, size.h).pad_xy(60.0 * k, 40.0 * k);
    if let Some(style) = ink_style(case) {
        let fluid = ink_layer(size, t, style);
        return root.child(fluid.view("ink")).child(layer).child(fluid.smear_view("smear"));
    }
    if case == "water pool" {
        return root.child(layer).child(water_pool(size, t).view("water"));
    }
    if case == "grid-words" {
        // A material on a panel also paints its children, so the grid is a sibling under the
        // card, across the whole surface.
        let m =
            Material::custom(GRID).reads_text().color(0, Color::hex(0x38e8ff, 1.0)).param(1, [1.3, 22.0 * k, 0.0, 0.0]);
        let grid = Element::panel("grid")
            .size(size.w, size.h)
            .fill(Color::WHITE)
            .material(m)
            .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute);
        return root.child(grid).child(layer);
    }
    let refracts = matches!(case, "glass lens" | "glass prism");
    if !case.contains("backdrop") && !case.starts_with("frost") && !case.starts_with("displace") && !refracts {
        return root.child(layer);
    }
    // Stripes under the card, so the tinted backdrop shows in the preview and the sheets.
    let stripes = Element::row("stripes")
        .children((0..(size.w / (12.0 * k)) as u32).map(|i| {
            let c = if i % 2 == 0 { Color::hex(0xe8ecf2, 1.0) } else { Color::hex(0x3a86ff, 1.0) };
            Element::panel(i as u64).size(12.0 * k, size.h).fill(c)
        }))
        .layout(|s| s.position = vo::scene::taffy::style::Position::Absolute);
    root.child(stripes).child(layer)
}

/// A capsule across the surface with seven bulges travelling along it, melted into one shape
/// group of eight members (EG-22), drawn as a shadow, a glow and a stroked fill.
fn shape_group_8(t: f64, rect: Rect, k: f32, p: &mut vo::scene::canvas::Painter) {
    use vo::scene::display::{GroupPrim, Paint, ShapeInstance};
    let pill = Rect::new(rect.x + 30.0 * k, rect.y + rect.h * 0.5 - 40.0 * k, rect.w - 60.0 * k, 80.0 * k);
    let mut prims = vec![GroupPrim::capsule(pill).with_ripple(1.0 * k, 40.0 * k, (t * 0.7) as f32)];
    for i in 0..7 {
        let u = ((i as f64 / 7.0 + t * 0.08) % 1.0) as f32;
        let grow = 0.5 + 0.5 * (t * 2.0 + i as f64).sin() as f32;
        prims.push(GroupPrim::bulge(pill, pill.x + u * pill.w, (50.0 + 30.0 * grow) * k, (90.0 + 50.0 * grow) * k));
    }
    let g = p.shape_group(&prims, 24.0 * k);
    let accent = Color::hex(0x4cc9f0, 1.0);
    p.shape(ShapeInstance {
        fill: Paint::shadow(Color::rgba(0.0, 0.0, 0.0, 0.6), Point::new(0.0, 6.0 * k), 10.0 * k, 0.0),
        ..g
    });
    p.shape(ShapeInstance { fill: Paint::glow(accent, 12.0 * k, 0.8), ..g });
    p.shape(
        ShapeInstance { fill: Paint::solid(Color::hex(0x16233a, 0.9)), ..g }.with_stroke(Paint::solid(accent), 1.5 * k),
    );
}

/// 20 word frames in four rows of five (EG-33), each a layer of its own turned toward the
/// middle and pushed further back than the one before it, the turn swaying. Drawn straight
/// into the target: no pane takes a texture.
fn panes_20(size: Size, t: f64) -> Element<Msg> {
    const WORDS: [&str; 20] = [
        "so", "the", "plan", "for", "tomorrow", "is", "to", "open", "the", "file", "and", "set", "the", "theme",
        "then", "read", "the", "live", "words", "back",
    ];
    let k = size.h / 300.0;
    let sway = 1.0 + 0.2 * (t * 1.7).sin() as f32;
    let pane = |i: usize| {
        let (col, row) = ((i % 5) as f32, (i / 5) as f32);
        Element::layer(format!("pane{i}"))
            .translate_z(-40.0 * k * i as f32)
            .rotate_y(0.35 * (col - 2.0) * sway)
            .rotate_x(0.25 * (row - 1.5))
            .child(
                Element::panel(format!("frame{i}"))
                    .size(100.0 * k, 46.0 * k)
                    .radius(9.0 * k)
                    .fill(Color::hex(0x10141c, 0.85))
                    .border(Color::hex(0x4cc9f0, 0.8), 1.5 * k)
                    .align_items(vo::scene::taffy::AlignItems::CENTER)
                    .justify(vo::scene::taffy::JustifyContent::CENTER)
                    .child(
                        Element::label(format!("word{i}"), WORDS[i])
                            .text_style(TextStyle::new(0, 20.0 * k, 24.0 * k))
                            .color(Color::hex(0xc8d3e0, 1.0)),
                    ),
            )
    };
    let row = |r: usize| Element::row(format!("row{r}")).gap(12.0 * k).children((0..5).map(move |c| pane(r * 5 + c)));
    Element::column("root")
        .size(size.w, size.h)
        .gap(14.0 * k)
        .align_items(vo::scene::taffy::AlignItems::CENTER)
        .justify(vo::scene::taffy::JustifyContent::CENTER)
        .children((0..4).map(row))
}

/// The glyph ids of a to z in the case font, shaped once.
fn alphabet() -> &'static [(u32, u32)] {
    static IDS: std::sync::OnceLock<Vec<(u32, u32)>> = std::sync::OnceLock::new();
    IDS.get_or_init(|| {
        let shaped =
            ShapedLayout::new(fonts()).layout("abcdefghijklmnopqrstuvwxyz", &TextStyle::new(0, 14.0, 18.0), None);
        shaped.glyphs.iter().map(|g| (g.font, g.glyph)).collect()
    })
}

/// 2000 letters at 14 px in a 50 x 40 grid, each turning at its own rate about its own
/// centre (EG-15): one glyph instance each, no layer per letter.
fn rotating_glyphs(t: f64, rect: Rect, k: f32, p: &mut vo::scene::canvas::Painter) {
    use vo::scene::display::{GlyphInstance, GlyphXform, NO_WORD};
    let ids = alphabet();
    let (cw, ch) = (rect.w / 50.0, rect.h / 40.0);
    for i in 0..2000 {
        let (font, glyph) = ids[i % ids.len()];
        let speed = 60.0 + (i % 7) as f64 * 25.0;
        p.glyph(GlyphInstance {
            pos: Point::new(rect.x + cw * (i % 50) as f32 + 0.2 * cw, rect.y + ch * (i / 50 + 1) as f32),
            size: 14.0 * k,
            font,
            glyph,
            word: NO_WORD,
            color: Color::hex(0x9be7ff, 0.85),
            xform: GlyphXform::rotate(((t * speed + i as f64 * 11.0) % 360.0) as f32),
        });
    }
}

/// 200 short pen strokes in a 20 x 10 grid, each a five-segment hook 1 px wide at its start
/// and 4 px at its end, through one width table each (SW-04).
fn tapered_strokes(t: f64, rect: Rect, k: f32, p: &mut vo::scene::canvas::Painter) {
    let (cw, ch) = (rect.w / 20.0, rect.h / 10.0);
    let wobble = (t * 2.0).sin() as f32;
    for i in 0..200 {
        let (x, y) = (rect.x + cw * (i % 20) as f32 + 0.15 * cw, rect.y + ch * (i / 20) as f32 + 0.2 * ch);
        let pts: Vec<Point> = (0..6)
            .map(|j| {
                let u = j as f32 / 5.0;
                Point::new(x + 0.7 * cw * u, y + 0.6 * ch * (u * 3.0 + 0.4 * wobble).sin().abs())
            })
            .collect();
        let c = Contour::polyline(&pts, false);
        let id = p.widths(&[WidthKnot::new(0.0, 0.25), WidthKnot::new(c.length(), 1.0)]);
        p.stroke(&c, &StrokeStyle::solid(4.0 * k, Color::hex(0xf2e6c8, 0.95)).widths(id));
    }
}

/// Three depth-stack sheets that frost the two older lines behind them (EG-04): the whole
/// frame has to stay under 1 ms at 600x300.
fn frost_stack(size: Size, k: f32) -> Element<Msg> {
    let stack = DepthStack {
        sheet: Some(Sheet {
            fill: Color::hex(0x16222c, 0.35),
            edge: Color::hex(0x9fd8ff, 0.35),
            edge_width: 1.0,
            radius: 6.0 * k,
            pad: (14.0 * k, 6.0 * k),
        }),
        step_z: 70.0 * k,
        step_rise: 18.0 * k,
        frost: 3.0 * k,
        ..DepthStack::default()
    };
    let lines: Vec<(vo::scene::Key, &str)> =
        ["the oldest line goes back", "an older line behind glass", "the newest line reads"]
            .iter()
            .map(|l| (vo::scene::Key::from(l.to_string()), *l))
            .collect();
    let content = |text: &&str, d: usize, _: &Place| {
        Element::label(format!("t{d}"), text.to_string())
            .text_style(TextStyle::new(0, 20.0 * k, 26.0 * k))
            .color(Color::hex(0xf2f3f5, 1.0))
    };
    let sheet = Size::new(size.w * 0.7, 40.0 * k);
    Element::panel("root")
        .size(size.w, size.h)
        .fill(Color::hex(0x0b0f14, 1.0))
        .child(stack.view("stack", size, sheet, &lines, content, false))
}

/// A pool of water (EG-10) below the card that mirrors it, with a drop every 0.3 s and four
/// rings alive at any time.
fn water_pool(size: Size, t: f64) -> WaterPool {
    let last = (t / 0.3).floor() * 0.3;
    let drops = (0..4).rev().map(|n| PoolDrop::word(last - n as f64 * 0.3, 0.15 + 0.23 * n as f32, 0.8));
    WaterPool::new(size, Rect::new(0.0, size.h * 0.7, size.w, size.h * 0.3))
        .light(false)
        .reduced_motion(false)
        .drops(drops.filter(|d| d.t >= 0.0))
}

/// The ink style of an `ink`, `smoke` or `watercolour` case.
fn ink_style(case: &str) -> Option<InkStyle> {
    match case {
        "ink" => Some(InkStyle::Ink),
        "smoke" => Some(InkStyle::Smoke),
        "watercolour" => Some(InkStyle::Watercolour),
        _ => None,
    }
}

/// A fluid layer (EG-12) behind the card: a word every 0.3 s along the card's text rows, the
/// newest word's row kept clear and the rows above it smeared. Smoke glows.
fn ink_layer(size: Size, t: f64, style: InkStyle) -> FluidLayer {
    let k = size.h / 300.0;
    let n = (t / 0.3).floor().max(0.0) as usize + 1;
    let rect = |i: usize| {
        Rect::new((90.0 + (i % 5) as f32 * 90.0) * k, (80.0 + (i / 5 % 2) as f32 * 34.0) * k, 70.0 * k, 26.0 * k)
    };
    let words = (0..n).map(|i| InkWord { t: i as f64 * 0.3, rect: rect(i), level: 0.4 + 0.15 * (i % 4) as f32 });
    let newest = rect(n - 1);
    let l = FluidLayer::new(size, Point::ZERO)
        .style(style)
        .light(false)
        .reduced_motion(false)
        .smear(0.08)
        .keep_clear(newest.y - 4.0 * k, newest.y + newest.h + 4.0 * k)
        .words(words);
    if style == InkStyle::Smoke {
        l.glow(0.8)
    } else {
        l
    }
}

/// GPU ms of the fluid's own work per 60 fps frame at `size`: two ticks and one output pass.
fn ink_sim_ms(g: &Gpu, size: Size) -> f64 {
    use vo_sim::fluid::{Fluid, FluidSplat};
    use vo_sim::Sim;
    let cfg = vo_sim::fluid_render::layer_config(&ink_layer(size, 0.0, InkStyle::Ink), 1.0);
    let mut fluid = Fluid::new(&g.device, &g.queue, cfg);
    fluid.apply(&FluidSplat {
        x: 0.3,
        y: 0.5,
        radius: 0.04,
        velocity: [0.2, -0.5],
        color: [0.2, 0.4, 1.0],
        amount: 1.0,
    });
    fluid.gpu_cost(2).map_or(0.0, |c| c.tick_ms * 2.0 + c.output_ms)
}

/// GPU ms of the water's own work per 60 fps frame at `size`: two ticks and one output pass.
fn water_sim_ms(g: &Gpu, size: Size) -> f64 {
    use vo_sim::Sim;
    let pool = water_pool(size, 0.0);
    let cfg = WaterConfig { reduced_motion: false, ..WaterConfig::for_size(pool.pool.w, pool.pool.h) };
    let mut water = Water::new(&g.device, &g.queue, cfg);
    water.apply(&WaterDrop { x: 0.3, y: 0.3, radius: 0.03, strength: -0.8 });
    water.gpu_cost(2).map_or(0.0, |c| c.tick_ms * 2.0 + c.output_ms)
}

fn fonts() -> FontRegistry {
    let fonts = FontRegistry::new();
    let path = "/usr/share/fonts/noto/NotoSans-Medium.ttf";
    fonts.add_file(path.as_ref()).unwrap_or_else(|e| panic!("cannot load {path}: {e}"));
    fonts
}

/// Cycles through the cases, for the preview and contact sheets.
struct Tour {
    size: Size,
    case: usize,
}

impl App for Tour {
    type Msg = Msg;

    fn init(&mut self, cx: &mut Cx<Msg>) {
        for i in 1..CASES.len() {
            cx.after(i as f64 * CASE_SECS, Msg::Case(i));
        }
    }

    fn update(&mut self, msg: Msg, _: &mut Cx<Msg>) {
        let Msg::Case(i) = msg;
        self.case = i;
    }

    fn view(&self, _: &vo::Tokens) -> Element<Msg> {
        view(self.size, self.case, 0.0)
    }
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    name: String,
}

fn gpu() -> Gpu {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
        apply_limit_buckets: false,
    }))
    .expect("no Vulkan adapter");
    let want = timestamp_features();
    let features = if adapter.features().contains(want) { want } else { wgpu::Features::empty() };
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("effect-cost"),
        required_features: features,
        ..Default::default()
    }))
    .expect("no device");
    Gpu { device, queue, name: adapter.get_info().name }
}

/// One case at one size.
struct Row {
    case: &'static str,
    gpu_p50: f64,
    gpu_p95: f64,
    cpu_p50: f64,
    slowest: String,
    pool_mb: f64,
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    v.sort_by(f64::total_cmp);
    v[((v.len() - 1) as f64 * q).round() as usize]
}

fn measure(g: &Gpu, fonts: &FontRegistry, size: Size, frames: usize, only: &[String]) -> Vec<Row> {
    let (w, h) = (size.w as u32, size.h as u32);
    let mut target = Target::new(&g.device, w, h, wgpu::TextureFormat::Rgba8UnormSrgb);
    let mut out = Vec::new();
    for (i, name) in CASES.iter().enumerate() {
        if i > 0 && !only.is_empty() && !only.iter().any(|o| o == name) {
            continue;
        }
        // A renderer per case, so pooled textures belong to the case.
        let mut r = Renderer::with_fonts(&g.device, &g.queue, fonts.clone());
        assert!(r.set_profiling(true), "this device has no timestamp queries");
        add_materials(&mut r);
        // The water's ticks run in their own submits, outside the frame's timestamps.
        let water_ms = if *name == "water pool" {
            vo_sim::water_render::install_pool(&mut r, &water_pool(size, 0.0), 1.0);
            water_sim_ms(g, size)
        } else if let Some(style) = ink_style(name) {
            vo_sim::fluid_render::install_layer(&mut r, &ink_layer(size, 0.0, style), 1.0);
            ink_sim_ms(g, size)
        } else {
            0.0
        };
        let mut scene: Scene<Msg> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())));
        scene.set_viewport(0.0, size, 1.0);
        let (mut gpu, mut cpu, mut sections) = (Vec::new(), Vec::new(), Vec::<(String, f64)>::new());
        // The text field (EG-31), built in its own submit, timed and added to the frame.
        let mut field = (*name == "grid-words")
            .then(|| (vo_render::text_field::TextField::new(&g.device, &g.queue), FieldClock::new(g)));
        // Ten frames to warm up pipelines and the atlas.
        for f in 0..frames + 10 {
            let t = f as f64 / 60.0;
            scene.apply(view(size, i, t), t);
            let list = scene.sample(t);
            let field_ms = match &mut field {
                Some((f, clock)) => clock.time(g, |enc| {
                    f.update_words(&mut r, &list, [w, h], 1.0, t, enc);
                }),
                None => 0.0,
            };
            if let Some((f, _)) = &field {
                f.bind(&mut r, &f.word_table(t, false));
            }
            let started = Instant::now();
            r.render_at(&list, 1.0, &mut target);
            let record = started.elapsed().as_secs_f64() * 1e3;
            let Some(tm) = r.last_timings() else { continue };
            if f >= 10 {
                gpu.push(tm.total_ms + water_ms + field_ms);
                cpu.push(record);
                for p in tm.passes {
                    match sections.iter_mut().find(|s| s.0 == p.label) {
                        Some(s) => s.1 += p.ms,
                        None => sections.push((p.label, p.ms)),
                    }
                }
            }
        }
        if let Some((f, _)) = &field {
            println!(
                "effect_cost: grid-words at {w}x{h}: field {:?} texels, {} KiB, built {} times in {} frames",
                f.size().unwrap_or_default(),
                f.memory_bytes() / 1024,
                f.builds(),
                frames + 10
            );
        }
        if *name == "panes-20" {
            let report = r.frame_report();
            let offscreen = report.surfaces.iter().filter(|s| s.kind == vo_render::profile::SurfaceKind::Layer).count();
            println!("effect_cost: panes-20 at {w}x{h}: {offscreen} offscreen layers, {} draws", report.surfaces[0].draws);
        }
        sections.sort_by(|a, b| b.1.total_cmp(&a.1));
        let slowest =
            sections.first().map(|(l, ms)| format!("{l} {:.3}", ms / gpu.len().max(1) as f64)).unwrap_or_default();
        out.push(Row {
            case: name,
            gpu_p50: pct(&mut gpu, 0.5),
            gpu_p95: pct(&mut gpu, 0.95),
            cpu_p50: pct(&mut cpu, 0.5),
            slowest,
            pool_mb: r.pool_bytes() as f64 / 1e6,
        });
    }
    out
}

/// Times GPU work recorded into one encoder with two timestamps, waiting for the result.
struct FieldClock {
    set: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
}

impl FieldClock {
    fn new(g: &Gpu) -> FieldClock {
        let set = g.device.create_query_set(&wgpu::QuerySetDescriptor {
            label: None,
            ty: wgpu::QueryType::Timestamp,
            count: 2,
        });
        let buf = |usage| {
            g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: 16, usage, mapped_at_creation: false })
        };
        use wgpu::BufferUsages as U;
        FieldClock { set, resolve: buf(U::QUERY_RESOLVE | U::COPY_SRC), read: buf(U::MAP_READ | U::COPY_DST) }
    }

    /// Milliseconds the GPU spent on what `record` encodes.
    fn time(&mut self, g: &Gpu, record: impl FnOnce(&mut wgpu::CommandEncoder)) -> f64 {
        let mut enc = g.device.create_command_encoder(&Default::default());
        enc.write_timestamp(&self.set, 0);
        record(&mut enc);
        enc.write_timestamp(&self.set, 1);
        enc.resolve_query_set(&self.set, 0..2, &self.resolve, 0);
        enc.copy_buffer_to_buffer(&self.resolve, 0, &self.read, 0, 16);
        let sub = g.queue.submit([enc.finish()]);
        g.device.poll(wgpu::PollType::Wait { submission_index: Some(sub), timeout: None }).expect("poll");
        self.read.slice(..).map_async(wgpu::MapMode::Read, |r| r.expect("map"));
        g.device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let ticks: Vec<u64> = {
            let d = self.read.slice(..).get_mapped_range().expect("mapped");
            d.as_chunks::<8>().0.iter().map(|c| u64::from_le_bytes(*c)).collect()
        };
        self.read.unmap();
        ticks[1].saturating_sub(ticks[0]) as f64 * g.queue.get_timestamp_period() as f64 / 1e6
    }
}

fn main() {
    if vo::preview::wanted() {
        let fonts = fonts();
        let size = Size::new(600.0, 300.0);
        let scenario = Scenario { size, scale: 1.0, duration: CASES.len() as f64 * CASE_SECS, inputs: Vec::new() };
        let options = move || Options::<Msg> {
            fonts: fonts.clone(),
            renderer_setup: Some(std::sync::Arc::new(move |r: &mut Renderer| {
                add_materials(r);
                vo_sim::water_render::install_pool(r, &water_pool(size, 0.0), 1.0);
                vo_sim::fluid_render::install_layer(r, &ink_layer(size, 0.0, InkStyle::Ink), 1.0);
            })),
            ..Options::default()
        };
        let mut p = vo::preview::Preview::new("effect_cost", move || Tour { size, case: 0 }, options, scenario);
        for (i, name) in CASES.iter().enumerate() {
            p = p.marker(name.replace(' ', "-"), i as f64 * CASE_SECS + 0.5);
        }
        std::process::exit(vo::preview::main(p));
    }
    let (mut frames, mut out, mut max_ms) = (120usize, None::<String>, None::<f64>);
    let mut only = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--frames" => frames = args.next().and_then(|v| v.parse().ok()).expect("--frames takes a number"),
            "--out" => out = args.next(),
            "--max-ms" => max_ms = args.next().and_then(|v| v.parse().ok()),
            "--case" => only.push(args.next().expect("--case takes a case name")),
            "--real-display" => {}
            _ => panic!("unknown option {a}"),
        }
    }
    let g = gpu();
    let fonts = fonts();
    let mut md = format!(
        "GPU time per frame by effect, {frames} frames each, on {}. Median and 95th percentile from \
         timestamp queries; +ms is the median over the plain card's. CPU is the time to record and \
         submit the frame.\n\n",
        g.name
    );
    let mut over = Vec::new();
    let mut slowest = Vec::new();
    for size in [Size::new(600.0, 300.0), Size::new(1920.0, 1080.0)] {
        let rows = measure(&g, &fonts, size, frames, &only);
        let base = rows[0].gpu_p50;
        md.push_str(&format!("### {}x{}\n\n", size.w, size.h));
        md.push_str("| effect | GPU ms p50 | p95 | +ms | CPU ms | slowest section (ms) | pool MB |\n");
        md.push_str("|---|---:|---:|---:|---:|---|---:|\n");
        for r in &rows {
            md.push_str(&format!(
                "| {} | {:.3} | {:.3} | {:+.3} | {:.2} | {} | {:.1} |\n",
                r.case,
                r.gpu_p50,
                r.gpu_p95,
                r.gpu_p50 - base,
                r.cpu_p50,
                r.slowest,
                r.pool_mb
            ));
            if size.w == 600.0 && max_ms.is_some_and(|m| r.gpu_p50 > m) {
                over.push(format!("{} {:.3} ms", r.case, r.gpu_p50));
            }
        }
        md.push('\n');
        if let Some(r) = rows.iter().max_by(|a, b| a.gpu_p50.total_cmp(&b.gpu_p50)) {
            slowest.push(format!("{}x{} {} {:.3} ms", size.w, size.h, r.case, r.gpu_p50));
        }
    }
    print!("{md}");
    if let Some(path) = out {
        std::fs::write(&path, &md).unwrap_or_else(|e| panic!("{path}: {e}"));
    }
    eprintln!("effect_cost: slowest: {}", slowest.join(", "));
    if !over.is_empty() {
        eprintln!("effect_cost: over {} ms at 600x300: {}", max_ms.unwrap_or(0.0), over.join(", "));
        std::process::exit(1);
    }
}
