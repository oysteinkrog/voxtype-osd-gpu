//! Vertical labels (FX-005) and autofit to a box (FX-049) as `TextStyle` fields.

use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontId, FontRegistry};
use vo_scene::geom::Size;
use vo_scene::text::{fit_size, ShapedLayout};
use vo_scene::Scene;

fn scene(fonts: &FontRegistry) -> Scene<()> {
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts.clone())))
}

fn jp(fonts: &FontRegistry) -> FontId {
    fonts.asset("jp-labels").unwrap()
}

#[test]
fn a_vertical_label_stacks_its_characters_in_one_column() {
    let fonts = FontRegistry::new();
    let style = TextStyle::new(jp(&fonts), 20.0, 24.0).vertical();
    let mut s = scene(&fonts);
    s.apply(Element::column("root").child(Element::label("v", "録音中").text_style(style)), 0.0);
    let list = s.sample(0.0);
    assert_eq!(list.glyphs.len(), 3);
    for pair in list.glyphs.windows(2) {
        // One em apart: the face's vertical advance, not the line height (EG-30).
        assert!((pair[1].pos.y - pair[0].pos.y - 20.0).abs() < 1e-3, "{:?}", pair[1].pos);
        assert!((pair[1].pos.x - pair[0].pos.x).abs() < 2.0, "one column");
    }
    // The box is one column wide and three ems tall.
    let r = s.sampled(s.find(&["root", "v"]).unwrap(), 0.0).unwrap().size;
    assert!(r.w > 19.99 && r.w < 24.0 && (r.h - 60.0).abs() < 1e-3, "{r:?}");
}

fn fit_view(text: &str, style: TextStyle) -> Element<()> {
    Element::column("root").child(Element::label("l", text).text_style(style).size(160.0, 50.0))
}

fn sizes(list: &DisplayList) -> Vec<f32> {
    list.glyphs.iter().map(|g| g.size).collect()
}

#[test]
fn a_label_with_fit_shrinks_to_its_box_in_steps() {
    let fonts = FontRegistry::new();
    let font = fonts.add_file(&test_font_path()).unwrap();
    let style = TextStyle::new(font, 40.0, 48.0).fit(10.0, 2.0);
    let text = "a longer status line";
    let want = fit_size(&ShapedLayout::new(fonts.clone()), text, &style, Size::new(160.0, 50.0), 10.0, 2.0).size;
    assert!((10.0..40.0).contains(&want));
    assert_eq!((40.0 - want) % 2.0, 0.0, "whole steps");

    let mut s = scene(&fonts);
    s.apply(fit_view(text, style), 0.0);
    let list = s.sample(0.0);
    assert!(sizes(&list).iter().all(|&z| z == want), "{:?} want {want}", sizes(&list));

    // Text that fits keeps the full size; without fit the size is the style's.
    s.apply(fit_view("ok", style), 1.0);
    assert!(sizes(&s.sample(1.0)).iter().all(|&z| z == 40.0));
    s.apply(fit_view(text, TextStyle::new(font, 40.0, 48.0)), 2.0);
    assert!(sizes(&s.sample(2.0)).iter().all(|&z| z == 40.0));
}
