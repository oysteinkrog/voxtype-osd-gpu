//! `Layer::opacity` is the layer's own value, relative to its parent layer. The renderer
//! multiplies it down the tree, so the product along a layer's parent chain is the opacity
//! the scene sampled for that node.

use vo_scene::display::{ItemKind, ShapeKind};
use vo_scene::element::Element;
use vo_scene::geom::Color;
use vo_scene::{DisplayList, Scene};

type El = Element<()>;

fn dot(key: &'static str) -> El {
    Element::shape(key, ShapeKind::Circle).size(10.0, 10.0).fill(Color::WHITE)
}

/// The opacity a layer is drawn with: the product along its parent chain.
fn chain(list: &DisplayList, mut layer: u32) -> f32 {
    let mut o = 1.0;
    loop {
        let l = &list.layers[layer as usize];
        o *= l.opacity;
        match l.parent {
            Some(p) => layer = p,
            None => return o,
        }
    }
}

#[test]
fn nested_layers_carry_their_own_opacity() {
    // outer layer 0.8, a plain stack at 0.5, inner layer 0.5, then a rotated dot at 0.5
    // (a rotation gets a direct layer of its own, which carries no opacity).
    let view = Element::column("root").child(
        Element::layer("outer").opacity(0.8).size(100.0, 100.0).child(
            Element::column("mid").opacity(0.5).child(
                Element::layer("inner")
                    .opacity(0.5)
                    .size(50.0, 50.0)
                    .child(dot("spun").rotate(0.3).opacity(0.5))
                    .child(dot("flat")),
            ),
        ),
    );
    let mut scene = Scene::new();
    scene.apply(view, 0.0);
    let list = scene.sample(0.0);

    assert_eq!(list.layers.len(), 4);
    let outer = &list.layers[1];
    let inner = &list.layers[2];
    let spun = &list.layers[3];
    assert_eq!((outer.parent, inner.parent, spun.parent), (Some(0), Some(1), Some(2)));
    assert_eq!(outer.opacity, 0.8);
    // The stack's 0.5 between the two layers is folded into the inner layer, once.
    assert_eq!(inner.opacity, 0.25);
    assert_eq!(spun.opacity, 1.0);

    let sampled = |path: &[&str]| scene.sampled(scene.find(path).unwrap(), 0.0).unwrap().opacity;
    let inner_path = ["root", "outer", "mid", "inner"];
    assert!((chain(&list, 2) - sampled(&inner_path)).abs() < 1e-6);

    // The rotated dot's own opacity goes into its colour, under a layer chain of 0.2.
    let spun_items = list.layer_items(3);
    assert_eq!(spun_items[0].kind, ItemKind::Shapes);
    let spun_shape = list.shapes[spun_items[0].start as usize];
    let drawn = chain(&list, 3) * spun_shape.fill.c0.a;
    assert!((drawn - sampled(&["root", "outer", "mid", "inner", "spun"])).abs() < 1e-6, "{drawn}");
}
