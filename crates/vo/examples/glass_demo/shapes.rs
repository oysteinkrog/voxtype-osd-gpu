//! Glass on any outline and on turned layers (EG-06), through the element path and the
//! renderer's display list pass: a chamfered octagon, a wavy slab, a prism face and a round
//! lens in dark and light glass, then a card turned 20 degrees about y and one rotated 8
//! degrees in the plane, over stripes. Each row's caption sits on the interior tint, so the
//! AX01 numbers printed are `Glass::contrast` for its text colour.
//!
//!   cargo run --release -p vo --example glass_demo -- --shapes /var/tmp/claude/eg06

use std::path::Path;

use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::glass::{Glass, GlassShape};
use vo::scene::taffy::style::{LengthPercentageAuto, Position};
use vo::scene::taffy::{AlignItems, JustifyContent};
use vo::scene::Scene;
use vo::ShapedLayout;
use vo_render::Offscreen;

use crate::{load_fonts, Look, DARK, LIGHT, SANS};

const W: f32 = 760.0;
const H: f32 = 300.0;

/// The shapes in a row, with their names.
fn shapes() -> [(&'static str, GlassShape); 4] {
    [
        ("octagon", GlassShape::chamfer(12.0)),
        ("wavy", GlassShape::Wavy { amplitude: 3.0, wavelength: 34.0 }),
        ("prism", GlassShape::Polygon { sides: 3, turn: 0.0 }),
        ("lens", GlassShape::Ellipse),
    ]
}

/// Stripes across the whole surface, out of the layout.
fn stripes<M: Clone + 'static>() -> Element<M> {
    Element::row("stripes")
        .children((0..(W / 20.0) as u32).map(|i| {
            let c = [0xe62832, 0xfac81e, 0x28b450, 0x1e78e6, 0x9632c8, 0xf0f0f0][i as usize % 6];
            Element::panel(i as u64).size(20.0, H).fill(Color::hex(c, 1.0))
        }))
        .layout(|s| {
            s.position = Position::Absolute;
            s.inset.left = LengthPercentageAuto::length(0.0);
            s.inset.top = LengthPercentageAuto::length(0.0);
        })
}

fn caption<M: Clone + 'static>(key: &str, text: &str, look: &Look) -> Element<M> {
    Element::label(key.to_string(), text).text_style(TextStyle::new(SANS, 15.0, 19.0)).color(look.foreground)
}

/// One sheet: the shapes in both looks on top, the turned cards below.
fn view<M: Clone + 'static>() -> Element<M> {
    let mut top = Element::row("shapes").gap(12.0).align_items(AlignItems::CENTER);
    for look in [&DARK, &LIGHT] {
        for (name, shape) in shapes() {
            let glass = look.glass.with_orbit(look.hues[0]).with_shape(shape);
            let (w, h) = if name == "prism" { (84.0, 76.0) } else { (70.0, 76.0) };
            top = top.child(
                Element::panel(format!("{}-{name}", look.name))
                    .size(w, h)
                    .radius(14.0)
                    .glass(glass)
                    .align_items(AlignItems::CENTER)
                    .justify(JustifyContent::CENTER)
                    .child(caption(&format!("{}-{name}-t", look.name), "Aa", look)),
            );
        }
    }
    let card = |key: &str, look: &Look, text: &str| {
        Element::panel(key.to_string())
            .size(240.0, 100.0)
            .radius(16.0)
            .pad(16.0)
            .glass(look.glass.with_orbit(look.hues[0]))
            .child(caption(&format!("{key}-t"), text, look))
    };
    let tilted = Element::layer("tilt").rotate_y(20f32.to_radians()).child(card("tilted", &DARK, "turned 20 degrees about y"));
    let rotated = Element::layer("turn").rotate(8f32.to_radians()).child(card("rotated", &LIGHT, "rotated 8 degrees"));
    let bottom = Element::row("cards").gap(60.0).child(tilted).child(rotated);
    Element::column("root")
        .size(W, H)
        .pad(20.0)
        .gap(36.0)
        .align_items(AlignItems::CENTER)
        .child(stripes())
        .child(top)
        .child(bottom)
}

/// Renders the sheet at 1x and 1.25x, during the sheen and with the orbit, to `dir`.
pub fn run(dir: &Path) {
    std::fs::create_dir_all(dir).expect("output folder");
    let g = crate::gpu();
    let fonts = load_fonts();
    let mut off = Offscreen::with_fonts(&g.device, &g.queue, fonts.clone());
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts)));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(), 0.0);
    for (t, scale) in [(0.3, 1.0f32), (3.0, 1.0), (3.0, 1.25)] {
        let list = scene.sample(t);
        let (w, h) = ((W * scale).ceil() as u32, (H * scale).ceil() as u32);
        let rgba = off.render_to_rgba(&list, [w, h], scale);
        let path = dir.join(format!("glass-shapes-t{t:.1}-{scale}x.png"));
        vo_render::offscreen::write_png(&path, w, h, &rgba).expect("png");
        println!("glass_demo: {} ({} glass panels)", path.display(), list.glass.len());
    }
    for look in [&DARK, &LIGHT] {
        let c = |text: Color| Glass::contrast(&look.glass, text, 0.02);
        println!(
            "glass_demo: AX01 {} glass on every shape (the tint is the same): body {:.2}:1, captions {:.2}:1",
            look.name,
            c(look.foreground),
            c(look.inactive)
        );
    }
}
