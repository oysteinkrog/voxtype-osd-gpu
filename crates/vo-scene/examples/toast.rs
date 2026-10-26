//! Builds a notification toast, applies it at t = 0, and prints the display list at a
//! time of your choice as JSON.
//!
//! `cargo run -p vo-scene --example toast -- 0.1`

use vo_core::spring::Spring;
use vo_scene::display::ShapeKind;
use vo_scene::element::{Element, TextStyle};
use vo_scene::geom::Color;
use vo_scene::{Prop, Scene, Transition};

#[derive(Clone, Debug)]
enum Msg {
    Dismiss,
}

fn toast(title: &str, body: &str) -> Element<Msg> {
    let close = Element::shape("close", ShapeKind::Circle)
        .size(18.0, 18.0)
        .fill(Color::hex(0xffffff, 0.12))
        .hairline(Color::hex(0xffffff, 0.4))
        .on_click(Msg::Dismiss);
    let header = Element::row("header")
        .gap(8.0)
        .child(Element::shape("dot", ShapeKind::Circle).size(10.0, 10.0).fill(Color::hex(0x4cc38a, 1.0)))
        .child(
            Element::label("title", title).text_style(TextStyle { font: 0, size: 15.0, line_height: 19.0 }).grow(1.0),
        )
        .child(close);
    Element::panel("toast")
        .width(320.0)
        .pad(14.0)
        .gap(6.0)
        .radius(14.0)
        .fill_gradient(Color::hex(0x24262b, 0.96), Color::hex(0x1b1c20, 0.96))
        .stroke(Color::hex(0xffffff, 0.08), 1.0)
        .motion(Prop::Size, Spring::SNAP)
        .presence(Transition::fade().offset(0.0, -12.0).scale(0.96), Transition::fade().scale(0.98))
        .interactive()
        .child(header)
        .child(Element::label("body", body).color(Color::hex(0xc8cad0, 1.0)))
        .child(Element::shape("rule", ShapeKind::Line).height(0.0).hairline(Color::hex(0xffffff, 0.1)))
}

fn main() {
    let t: f64 = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(0.1);
    let mut scene = Scene::new();
    scene.apply(toast("Saved", "The recording was transcribed and copied to the clipboard."), 0.0);
    let list = scene.sample(t);
    println!("{}", serde_json::to_string_pretty(&list).expect("display lists serialize"));
    eprintln!(
        "t = {t}: {} layers, {} shapes, {} glyphs, input region {:?}, next wake {:?}",
        list.layers.len(),
        list.shapes.len(),
        list.glyphs.len(),
        list.input_region().rects,
        scene.next_wake(t)
    );
}
