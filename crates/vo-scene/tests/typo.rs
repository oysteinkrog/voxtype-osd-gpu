//! Per-word typographic channels (MO-03) from the element API into the display list: weight
//! as a glyph effect, baseline and tracking as glyph moves, ink and fill as opacity, and the
//! reading-line envelope on the newest line.

use vo_core::words::Word;
use vo_scene::display::DisplayList;
use vo_scene::element::{Element, TextStyle};
use vo_scene::font::{test_font_path, FontRegistry};
use vo_scene::text::ShapedLayout;
use vo_scene::typo::{Typo, Typos, WordTypo};
use vo_scene::Scene;

fn scene() -> Scene<()> {
    let fonts = FontRegistry::new();
    fonts.add_file(&test_font_path()).unwrap();
    Scene::new().with_text(Box::new(ShapedLayout::new(fonts)))
}

fn words(text: &str) -> Vec<Word> {
    text.split(' ').enumerate().map(|(i, w)| Word { id: i as u32 + 1, text: w.into() }).collect()
}

fn view(text: &str, width: f32, typos: Option<Typos>) -> Element<()> {
    let style = TextStyle { size: 20.0, line_height: 26.0, ..TextStyle::default() };
    let mut live = Element::live_text("t", &words(text)).text_style(style).width(width);
    if let Some(t) = typos {
        live = live.word_typo(t);
    }
    Element::column("root").child(live)
}

/// The glyphs of word `id`: `(x, y, alpha, weight)`.
fn glyphs(list: &DisplayList, id: u32) -> Vec<(f32, f32, f32, f32)> {
    let weight = |i: usize| list.glyph_fx.iter().find(|(g, _)| *g as usize == i).map_or(0.0, |e| e.1.weight);
    (0..list.glyphs.len())
        .filter(|&i| list.glyphs[i].word == id)
        .map(|i| {
            let g = &list.glyphs[i];
            (g.pos.x, g.pos.y, g.color.a, weight(i))
        })
        .collect()
}

#[test]
fn channels_reach_the_glyphs_and_settle() {
    let plain = {
        let mut s = scene();
        s.apply(view("one two", 400.0, None), 0.0);
        s.sample(5.0)
    };
    let mut s = scene();
    let typos = Typos::new().word(2, WordTypo::rest(Typo::REST.weight(0.4).baseline(2.0).tracking(1.04).ink(0.8)));
    s.apply(view("one two", 400.0, Some(typos)), 0.0);
    let list = s.sample(5.0);
    let (a, b) = (glyphs(&plain, 2), glyphs(&list, 2));
    assert_eq!(a.len(), 3);
    for (p, q) in a.iter().zip(&b) {
        assert!((q.1 - (p.1 - 2.0)).abs() < 1e-3, "baseline up 2 px: {p:?} {q:?}");
        assert!((q.2 - 0.8 * p.2).abs() < 1e-3, "ink: {p:?} {q:?}");
        assert!((q.3 - 0.4).abs() < 1e-3, "weight: {q:?}");
    }
    // Tracking spreads the letters about the word's middle: the first moves left, the last
    // right, each by 4 % of its centre's distance from the middle (about 0.5 px here).
    let (l, r) = (b[0].0 - a[0].0, b[2].0 - a[2].0);
    assert!(l < -0.3 && r > 0.3 && (l + r).abs() < 0.15, "{l} {r}");
    // Word one is untouched.
    assert_eq!(glyphs(&plain, 1), glyphs(&list, 1));
}

#[test]
fn the_reading_line_is_clamped_and_older_lines_are_not() {
    // Narrow enough that "alpha" and "beta" sit on two lines, "beta" newest.
    let wild = WordTypo::rest(Typo::REST.baseline(8.0).ink(0.3));
    let typos = Typos::new().word(1, wild).word(2, wild);
    let plain = {
        let mut s = scene();
        s.apply(view("alpha beta", 60.0, None), 0.0);
        s.sample(5.0)
    };
    let mut s = scene();
    s.apply(view("alpha beta", 60.0, Some(typos)), 0.0);
    let list = s.sample(5.0);
    let (old0, old1) = (glyphs(&plain, 1)[0], glyphs(&list, 1)[0]);
    let (new0, new1) = (glyphs(&plain, 2)[0], glyphs(&list, 2)[0]);
    assert!(new0.1 > old0.1, "beta is on a later line");
    assert!((old1.1 - (old0.1 - 8.0)).abs() < 1e-3, "older line moves the full 8 px");
    assert!((new1.1 - (new0.1 - 3.0)).abs() < 1e-3, "reading line clamps to 3 px");
    assert!((new1.2 - 0.7 * new0.2).abs() < 1e-3, "reading line ink at least 70 %");
    assert!((old1.2 - 0.3 * old0.2).abs() < 1e-3);
}

#[test]
fn arrive_kicks_and_wakes_the_scene() {
    let mut s = scene();
    s.apply(view("one", 400.0, Some(Typos::new())), 0.0);
    let start = Typo::REST.weight(0.6);
    let typos = Typos::new().word(2, WordTypo::rest(Typo::REST).arrive(start));
    s.apply(view("one two", 400.0, Some(typos)), 1.0);
    let w = |t: f64| glyphs(&s.sample(t), 2)[0].3;
    assert!((w(1.0) - 0.6).abs() < 1e-3);
    assert!(w(1.09) < 0.6 && w(1.09) > 0.0);
    assert!(s.next_wake(1.05).is_some());
    assert_eq!(w(2.0), 0.0);
}
