//! Refracting glass (EG-05): the four presets (lens disc, prism, thick slab, bubble with
//! thin-film iridescence) over a HUD ring, stripes and a line of text, through the element
//! path. Each sits in a layer that captures its backdrop, which the glass reads bent. Dark
//! glass by default; light glass and light-mode shading with `--light` or `VO_LIGHT=1`. The
//! AX01 numbers printed are `Glass::contrast` for each preset's caption colour.
//!
//!   cargo run --release -p vo --example glass_demo -- --refract /var/tmp/claude/eg05 [--light]

use std::path::Path;

use vo::scene::display::BackdropRes;
use vo::scene::element::{Element, TextStyle};
use vo::scene::geom::{Color, Size};
use vo::scene::glass::{Glass, GlassKind};
use vo::scene::taffy::style::{LengthPercentageAuto, Position};
use vo::scene::taffy::{AlignItems, JustifyContent};
use vo::scene::Scene;
use vo::ShapedLayout;
use vo_render::Offscreen;

use crate::{load_fonts, SANS};

const W: f32 = 760.0;
const H: f32 = 240.0;

fn at<M: Clone + 'static>(e: Element<M>, x: f32, y: f32) -> Element<M> {
    e.layout(|s| {
        s.position = Position::Absolute;
        s.inset.left = LengthPercentageAuto::length(x);
        s.inset.top = LengthPercentageAuto::length(y);
    })
}

/// The presets with their boxes.
fn presets(kind: GlassKind) -> [(&'static str, Glass, f32, f32); 4] {
    [
        ("lens", Glass::lens(kind), 150.0, 150.0),
        ("prism", Glass::prism(kind), 170.0, 150.0),
        ("slab", Glass::slab(kind), 160.0, 110.0),
        ("bubble", Glass::bubble(kind), 150.0, 150.0),
    ]
}

/// What the glass bends: per cell a ring of 12 ticks, stripes and a line of text.
fn under<M: Clone + 'static>(kind: GlassKind) -> Vec<Element<M>> {
    let ink = if kind == GlassKind::Dark { Color::hex(0xe8ecf2, 1.0) } else { Color::hex(0x10141c, 1.0) };
    let mut out = Vec::new();
    for i in 0..(W / 24.0) as u32 {
        let c = [0x3a86ff, 0xff6b6b, 0x2ec4b6][i as usize % 3];
        out.push(at(Element::panel(format!("stripe{i}")).size(6.0, H).fill(Color::hex(c, 0.55)), i as f32 * 24.0, 0.0));
    }
    for cell in 0..4 {
        let (cx, cy) = (95.0 + cell as f32 * 190.0, 110.0);
        let r = 40.0;
        out.push(at(
            Element::panel(format!("ring{cell}")).size(2.0 * r, 2.0 * r).radius(r).border(ink, 1.5),
            cx - r,
            cy - r,
        ));
        for t in 0..12 {
            let a = t as f32 * std::f32::consts::TAU / 12.0;
            out.push(at(
                Element::panel(format!("tick{cell}-{t}")).size(5.0, 5.0).fill(ink),
                cx + r * a.cos() - 2.5,
                cy + r * a.sin() - 2.5,
            ));
        }
    }
    out.push(at(
        Element::label("line", "the newest line reads through the glass, bent but whole")
            .text_style(TextStyle::new(SANS, 16.0, 20.0))
            .color(ink),
        24.0,
        200.0,
    ));
    out
}

fn view<M: Clone + 'static>(kind: GlassKind) -> Element<M> {
    let back = if kind == GlassKind::Dark { Color::hex(0x0b0d12, 1.0) } else { Color::hex(0xf3f4f6, 1.0) };
    let mut root = Element::panel("root").size(W, H).fill(back);
    for e in under(kind) {
        root = root.child(e);
    }
    let text = Glass::preset_texts(kind)[0];
    let mut fx = Element::layer("fx").size(W, H).backdrop(BackdropRes::Full, Color::TRANSPARENT);
    for (cell, (name, glass, w, h)) in presets(kind).into_iter().enumerate() {
        let (cx, cy) = (95.0 + cell as f32 * 190.0, 110.0);
        let panel = Element::panel(name)
            .size(w, h)
            .radius(18.0)
            .glass(glass)
            .align_items(AlignItems::CENTER)
            .justify(JustifyContent::CENTER)
            .child(Element::label(format!("{name}-t"), name).text_style(TextStyle::new(SANS, 16.0, 20.0)).color(text));
        fx = fx.child(at(panel, cx - 0.5 * w, cy - 0.5 * h));
    }
    root.child(at(fx, 0.0, 0.0))
}

/// Renders the sheet at two times (the bubble wobbles) and at 1.25x, to `dir`.
pub fn run(dir: &Path, light: bool) {
    std::fs::create_dir_all(dir).expect("output folder");
    let kind = if light { GlassKind::Light } else { GlassKind::Dark };
    let mode = if light { "light" } else { "dark" };
    let g = crate::gpu();
    let fonts = load_fonts();
    let mut off = Offscreen::with_fonts(&g.device, &g.queue, fonts.clone());
    off.renderer.set_light(light);
    let mut scene: Scene<()> = Scene::new().with_text(Box::new(ShapedLayout::new(fonts)));
    scene.set_viewport(0.0, Size::new(W, H), 1.0);
    scene.apply(view(kind), 0.0);
    for (t, scale) in [(1.0, 1.0f32), (2.5, 1.0), (2.5, 1.25)] {
        let list = scene.sample(t);
        let (w, h) = ((W * scale).ceil() as u32, (H * scale).ceil() as u32);
        let rgba = off.render_to_rgba(&list, [w, h], scale);
        let path = dir.join(format!("glass-refract-{mode}-t{t:.1}-{scale}x.png"));
        vo_render::offscreen::write_png(&path, w, h, &rgba).expect("png");
        println!("glass_demo: {} ({} glass panels)", path.display(), list.glass.len());
    }
    for (name, glass, _, _) in presets(kind) {
        let [body, caption] = Glass::preset_texts(kind).map(|t| glass.contrast(t, 0.02));
        println!(
            "glass_demo: AX01 {mode} {name}: tint alpha {:.3}, body {body:.2}:1, captions {caption:.2}:1",
            glass.tint.a
        );
    }
}
