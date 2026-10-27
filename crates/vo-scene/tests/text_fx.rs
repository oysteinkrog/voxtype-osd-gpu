//! Text effects (TY17) from the element API into the display list: label slots, a wipe
//! front across the line, and words that blur, dissolve or wipe in.

use vo_core::words::Word;
use vo_scene::display::{DisplayList, GlyphFx};
use vo_scene::element::Element;
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::geom::Color;
use vo_scene::text::ShapedLayout;
use vo_scene::{Scene, Transition};

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

/// Each glyph's effects, `None` for a glyph without any.
fn fx_per_glyph(list: &DisplayList) -> Vec<Option<GlyphFx>> {
    (0..list.glyphs.len() as u32).map(|i| list.glyph_fx.iter().find(|(g, _)| *g == i).map(|e| e.1)).collect()
}

#[test]
fn a_plain_label_has_no_glyph_effects() {
    let mut s = scene();
    s.apply(Element::column("root").child(Element::label("l", "plain")), 0.0);
    let list = s.sample(1.0);
    assert_eq!(list.glyphs.len(), 5);
    assert!(list.glyph_fx.is_empty());
}

#[test]
fn label_effects_reach_every_glyph() {
    let mut s = scene();
    let label = Element::label("l", "halo")
        .text_blur(2.0)
        .text_chroma(1.5)
        .text_dissolve(0.25)
        .text_halo(Color::rgba(0.2, 0.6, 1.0, 0.8), 6.0);
    s.apply(Element::column("root").child(label), 0.0);
    // Effect slots start at zero and spring to their values.
    let list = s.sample(10.0);
    let fx = fx_per_glyph(&list);
    assert_eq!(fx.len(), 4);
    for e in fx {
        let e = e.expect("every glyph has effects");
        assert!((e.blur - 2.0).abs() < 1e-3 && (e.chroma - 1.5).abs() < 1e-3, "{e:?}");
        assert!((e.dissolve - 0.25).abs() < 1e-3 && (e.halo_radius - 6.0).abs() < 1e-3);
        assert!((e.halo.a - 0.8).abs() < 1e-3);
        assert_eq!(e.wipe, 0.0);
    }
}

#[test]
fn a_label_wipe_moves_one_front_along_the_line() {
    let mut s = scene();
    s.apply(Element::column("root").child(Element::label("l", "reveal wipe").text_wipe(0.5)), 0.0);
    let list = s.sample(10.0);
    let wipes: Vec<f32> = fx_per_glyph(&list).iter().map(|e| e.map_or(0.0, |e| e.wipe)).collect();
    // Left glyphs show, right glyphs hide, and the one under the front is in between.
    assert_eq!(wipes[0], 0.0);
    assert_eq!(*wipes.last().unwrap(), 1.0);
    assert!(wipes.windows(2).all(|w| w[0] <= w[1]), "{wipes:?}");
}

#[test]
fn a_label_blurs_in_through_its_enter_transition() {
    let mut s = scene();
    let view = || {
        Element::column("root").child(Element::label("l", "hi").presence(Transition::NONE.blur(8.0), Transition::NONE))
    };
    s.apply(view(), 0.0);
    let blur = |t| fx_per_glyph(&s.sample(t))[0].map_or(0.0, |e| e.blur);
    assert!((blur(0.0) - 8.0).abs() < 1e-3);
    let mid = blur(0.1);
    assert!(mid > 0.0 && mid < 8.0, "{mid}");
    assert!(s.sample(5.0).glyph_fx.is_empty(), "sharp at rest");
}

#[test]
fn live_words_enter_with_their_own_effects() {
    let mut s = scene();
    let view = |words: &[Word]| {
        Element::column("root")
            .child(Element::live_text("live", words).word_enter(Transition::NONE.blur(6.0).dissolve(1.0).wipe(1.0)))
    };
    let w = |id, t: &str| Word { id, text: t.into() };
    s.apply(view(&[w(1, "open")]), 0.0);
    s.apply(view(&[w(1, "open"), w(2, "door")]), 5.0);
    let list = s.sample(5.0);
    let fx = fx_per_glyph(&list);
    for (g, e) in list.glyphs.iter().zip(&fx) {
        if g.word == 1 {
            assert!(e.is_none(), "the old word is at rest");
        } else {
            let e = e.expect("the new word has effects");
            assert!((e.blur - 6.0).abs() < 1e-3 && e.dissolve == 1.0 && e.wipe == 1.0, "{e:?}");
        }
    }
    // Part way in, the new word's wipe front sits inside it: its first glyph shows more than
    // its last.
    let list = s.sample(5.08);
    let fx = fx_per_glyph(&list);
    let wipes: Vec<f32> =
        list.glyphs.iter().zip(&fx).filter(|(g, _)| g.word == 2).map(|(_, e)| e.map_or(0.0, |e| e.wipe)).collect();
    assert!(wipes[0] < *wipes.last().unwrap(), "{wipes:?}");
    assert!(s.sample(10.0).glyph_fx.is_empty());
}

#[test]
fn shadow_and_outline_reach_every_glyph() {
    let mut s = scene();
    let shadow = Element::label("s", "drop").text_shadow(Color::rgba(0.0, 0.0, 0.0, 0.5), 1.0, 2.0, 6.0);
    let outline = Element::label("o", "edge").text_outline(1.5);
    s.apply(Element::column("root").child(shadow).child(outline), 0.0);
    let fx = fx_per_glyph(&s.sample(10.0));
    assert_eq!(fx.len(), 8);
    for e in &fx[..4] {
        let e = e.expect("a shadow is a glyph effect");
        assert!((e.halo_offset.x - 1.0).abs() < 1e-3 && (e.halo_offset.y - 2.0).abs() < 1e-3, "{e:?}");
        // The blur is about twice the halo radius, as CSS blurs.
        assert!((e.halo_radius - 3.0).abs() < 1e-3 && (e.halo.a - 0.5).abs() < 1e-3);
        assert_eq!(e.outline, 0.0);
    }
    for e in &fx[4..] {
        let e = e.expect("an outline is a glyph effect");
        assert!((e.outline - 1.5).abs() < 1e-3 && e.halo_radius == 0.0, "{e:?}");
    }
}
